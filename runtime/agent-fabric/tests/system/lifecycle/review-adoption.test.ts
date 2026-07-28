import Database from "better-sqlite3";
import { describe, expect, it } from "vitest";

import { applyMigrations } from "../../../src/core/migrations.ts";
import {
  LifecycleReviewAdoptionStore,
  type PreparedReviewAdoptionContext,
} from "../../../src/lifecycle/review-adoption.ts";
import { canonicalJson } from "../../../src/persistence/row-codec.ts";
import { custodyRef, lifecycleDigest } from "../../../src/lifecycle/custody-codec.ts";
import { LifecycleRotationRepository } from "../../../src/lifecycle/rotation-repository.ts";
import { admitProviderActionFixture } from "../../support/provider-action-fixture.ts";

const SHA = `sha256:${"a".repeat(64)}`;

function finalizedCustodyOwnerRef(): Readonly<Record<string, unknown>> {
  return {
    kind: "custody",
    custodyRef: custodyRef("run", "chair", "custody", 6),
    sourceRefDigest: lifecycleDigest("custody-semantic", {
      schemaVersion: 1,
      custodyId: "custody",
      revision: 6,
      state: "finalized",
      disposition: "adopted",
      proofKind: "provider-terminal",
      terminalEvidenceDigest: "terminal-evidence",
    }),
  };
}

function seedLifecycleScope(database: Database.Database): string {
  const scope = {
    schemaVersion: 1,
    projectId: "project",
    projectSessionId: "session",
    runId: "run",
    authorityId: "authority",
  };
  const admissionDigest = lifecycleDigest("admission", scope);
  const scopeDigest = lifecycleDigest("admitted-scope", scope);
  const requestId = lifecycleDigest("scope-admission-outbox", scope);
  const orderedRecordSetDigest = lifecycleDigest("scope-record-set", []);
  const checkpointBody = {
    schemaVersion: 1,
    authorityId: "authority",
    projectSessionId: "session",
    runId: "run",
    receiptCountDec: "0",
    headAuthoritySequenceDec: "0",
    headReceiptDigest: null,
    orderedRecordSetDigest,
  };
  const checkpointDigest = lifecycleDigest("scope-checkpoint", checkpointBody);
  const namespaceMember = {
    projectSessionId: "session",
    runId: "run",
    authorityId: "authority",
    scopeCheckpointDigest: checkpointDigest,
    receiptCountDec: "0",
    headReceiptDigest: null,
  };
  const orderedScopeHeadSetDigest = lifecycleDigest("namespace-scope-head-set", [namespaceMember]);
  const namespaceBody = {
    schemaVersion: 1,
    authorityId: "authority",
    projectId: "project",
    scopeCountDec: "1",
    orderedScopeHeadSetDigest,
  };
  const namespaceDigest = lifecycleDigest("namespace-checkpoint", namespaceBody);
  const resolutionBody = {
    schemaVersion: 1,
    admissionRequestId: requestId,
    scopeDigest,
    initialScopeCheckpointDigest: checkpointDigest,
    namespaceCheckpointDigest: namespaceDigest,
  };
  const resolutionDigest = lifecycleDigest("scope-admission-resolution", resolutionBody);
  database.prepare("INSERT INTO lifecycle_receipt_projects VALUES ('project','authority',1)").run();
  database.prepare("INSERT INTO lifecycle_scope_admission_outbox VALUES (?,?,?,?,?,?,?,?,?,?)").run(
    requestId, "project", "session", "run", "authority", admissionDigest, 1,
    canonicalJson(scope), scopeDigest, 1,
  );
  database.prepare("INSERT INTO lifecycle_admitted_run_scopes VALUES (?,?,?,?,?,?,?,?,?,?)").run(
    "project", "session", "run", "authority", admissionDigest, 1,
    requestId, scopeDigest, checkpointDigest, resolutionDigest,
  );
  database.prepare("INSERT INTO lifecycle_receipt_scope_checkpoints VALUES (?,?,?,?,?,?,?,?,?,?,?)").run(
    "session", "run", "authority", 0, 0, null, orderedRecordSetDigest,
    canonicalJson(checkpointBody), checkpointDigest, "scope-attestation", 1,
  );
  database.prepare("INSERT INTO lifecycle_receipt_scope_heads VALUES ('session','run',?,1)").run(checkpointDigest);
  database.prepare("INSERT INTO lifecycle_receipt_namespace_checkpoints VALUES (?,?,?,?,?,?,?,?)").run(
    "project", "authority", 1, orderedScopeHeadSetDigest, canonicalJson(namespaceBody),
    namespaceDigest, "namespace-attestation", 1,
  );
  database.prepare("INSERT INTO lifecycle_receipt_namespace_members VALUES (?,?,?,?,?,?,?,?,?)").run(
    "project", namespaceDigest, 1, "session", "run", "authority", checkpointDigest, 0, null,
  );
  database.prepare("INSERT INTO lifecycle_receipt_namespace_heads VALUES (?,?,?,?,?,1)").run(
    "project", "authority", 1, orderedScopeHeadSetDigest, namespaceDigest,
  );
  database.prepare("INSERT INTO lifecycle_scope_admission_resolutions VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)").run(
    requestId, "project", "session", "run", "authority", admissionDigest, 1, scopeDigest,
    0, 0, orderedRecordSetDigest, canonicalJson(checkpointBody), checkpointDigest, 1,
    namespaceDigest, canonicalJson(namespaceMember), 1, canonicalJson(resolutionBody), resolutionDigest,
  );
  return checkpointDigest;
}

function openReviewDatabase(): Database.Database {
  const database = new Database(":memory:");
  applyMigrations(database);
  database.pragma("foreign_keys = OFF");
  database.prepare(`
    INSERT INTO projects(project_id,canonical_root,revision,authority_generation,created_at,updated_at)
    VALUES ('project','/project',1,1,1,1)
  `).run();
  database.prepare(`
    INSERT INTO project_sessions(
      project_session_id,project_id,mode,state,revision,generation,authority_ref,budget_ref,
      launch_packet_path,launch_packet_digest,membership_revision,origin_kind,
      origin_operator_id,created_at,updated_at
    ) VALUES ('session','project','coordinated','active',1,1,'authority','budget',
              'launch','launch-digest',1,'operator-launch','operator',1,1)
  `).run();
  database.prepare(`
    INSERT INTO runs(
      run_id,chair_agent_id,workspace_root,created_at,project_session_id,lifecycle_state,
      revision,chair_generation,chair_lease_id,authority_ref,budget_ref,dependency_revision,
      topology_slot
    ) VALUES ('run','chair','/project',1,'session','active',1,1,'lease','authority','budget',1,1)
  `).run();
  database.prepare(`
    INSERT INTO authorities(authority_id,run_id,authority_json,authority_hash,created_at)
    VALUES ('authority','run','{}','authority-hash',1)
  `).run();
  database.prepare("INSERT INTO agents(run_id,agent_id,authority_id,lifecycle) VALUES ('run','chair','authority','ready')").run();
  database.prepare(`
    INSERT INTO tasks(run_id,task_id,authority_id,objective,base_revision,state,
                      owner_agent_id,revision,owner_lease_generation,created_by)
    VALUES ('run','task','authority','review','base','active','chair',1,1,'chair')
  `).run();
  admitProviderActionFixture(database, {
    runId: "run",
    adapterId: "replacement-adapter",
    actionId: "replacement-action",
    operation: "spawn",
    targetAgentId: "chair",
    identityHash: "identity",
    payloadHash: "payload",
    payloadJson: "{}",
    status: "terminal",
    historyJson: "[]",
    executionCount: 1,
    effectCount: 1,
    idempotencyProven: true,
    resultJson: "{}",
    updatedAt: 1,
  });
  database.prepare(`
    INSERT INTO artifacts(
      artifact_id,project_id,project_session_id,run_id,task_id,publisher_kind,
      publisher_ref,publisher_agent_id,source_kind,evidence_kind,relative_path,sha256,
      registry_state,quarantine_reason,revision,created_at
    ) VALUES ('artifact','project','session','run','task','project','project',NULL,
              'project-file','artifact','review/artifact.txt',?,'active',NULL,1,1)
  `).run(SHA);
  database.prepare(`
    INSERT INTO artifact_publication_lineage(
      artifact_id,artifact_revision,run_id,publisher_agent_id,publisher_principal_generation,
      publisher_bridge_generation,provider_custody_adapter_id,provider_custody_action_id,
      provider_session_generation,adapter_contract_digest,provider_family,model,
      route_receipt_digest,state,reason,lineage_digest,created_at
    ) VALUES ('artifact',1,'run','chair',1,1,'replacement-adapter','replacement-action',
              1,'source-contract','claude','model','route-receipt','proved',NULL,'lineage',1)
  `).run();
  database.prepare(`
    INSERT INTO delivery_review_bases(
      run_id,delivery_run_id,review_basis_revision,manifest_artifact_id,
      manifest_artifact_revision,manifest_digest,snapshot_digest,profile_digest,
      repository_source_state_digest,requirement_map_digest,evidence_closure_digest,
      current,basis_digest
    ) VALUES ('run','delivery',1,'artifact',1,'manifest','snapshot','profile-digest',
              'repository-source','requirements','evidence',1,'review-basis')
  `).run();
  database.prepare(`
    INSERT INTO review_bundles(
      run_id,bundle_generation,delivery_run_id,review_basis_revision,review_basis_digest,
      delivery_artifact_id,delivery_artifact_revision,base_object_id,head_object_id,
      head_tree_id,index_tree_id,review_diff_codec_digest,review_diff_rules_digest,
      review_diff_set_digest,repository_source_state_digest,publication_lineage_digest,
      coverage_digest,manifest_body_digest,manifest_root_digest,bundle_digest,
      bundle_search_index_digest,risk_read_map_digest,mandatory_read_set_digest,
      mandatory_read_count,mandatory_read_bytes,changed_path_count,required_evidence_count,
      carried_finding_count,object_count,chunk_count,total_object_bytes,manifest_page_bytes,
      search_index_bytes,risk_map_bytes,private_manifest_body_path,
      private_manifest_root_path,private_bundle_ref_path,created_at
    ) VALUES ('run',1,'delivery',1,'review-basis','artifact',1,'base','head','head-tree',
              'index-tree','codec','rules','diff-set','repository-source','lineage','coverage',
              'manifest-body','manifest-root','bundle','search-index','risk-map','mandatory-set',
              0,0,0,0,0,0,0,0,0,0,0,'manifest-body-path','manifest-root-path','bundle-ref',1)
  `).run();
  database.prepare(`
    INSERT INTO lifecycle_rotation_custodies(
      project_session_id,run_id,agent_id,custody_id,command_id,admission_digest,
      provider_action_adapter_id,provider_action_id,recovery_source_kind,bridge_owner_kind,
      checkpoint_validation_key,creation_json,creation_digest,created_at
    ) VALUES ('session','run','chair','custody','command','admission',
              'replacement-adapter','replacement-action','none','chair',
              'none','{}','custody-creation',1)
  `).run();
  database.prepare(`
    INSERT INTO lifecycle_rotation_custody_revisions(
      project_session_id,run_id,agent_id,custody_id,revision,prior_revision,
      prior_journal_digest,state,disposition_code,proof_kind,terminal_evidence_digest,
      semantic_json,semantic_digest,source_ref_digest,journal_json,journal_digest,recorded_at
    ) VALUES ('session','run','chair','custody',1,NULL,NULL,
              'awaiting-boundary','none','none',NULL,'{}','custody-semantic',
              'custody-source','{}','custody-journal',1)
  `).run();
  database.prepare(`
    INSERT INTO lifecycle_rotation_custody_heads(
      project_session_id,run_id,agent_id,custody_id,current_revision,state,
      disposition_code,semantic_digest,source_ref_digest,journal_digest,terminal,head_revision
    ) VALUES ('session','run','chair','custody',1,'awaiting-boundary','none',
              'custody-semantic','custody-source','custody-journal',0,1)
  `).run();
  const rotations = new LifecycleRotationRepository(database);
  let head = database.transaction(() => rotations.appendInCurrentTransaction({
    runId: "run", agentId: "chair", custodyId: "custody",
    expectedRevision: 1, state: "prepared", recordedAt: 2,
  }))();
  head = database.transaction(() => rotations.appendInCurrentTransaction({
    runId: "run", agentId: "chair", custodyId: "custody",
    expectedRevision: head.revision, state: "dispatched", recordedAt: 3,
  }))();
  head = database.transaction(() => rotations.appendInCurrentTransaction({
    runId: "run", agentId: "chair", custodyId: "custody",
    expectedRevision: head.revision, state: "provider-terminal",
    terminalEvidenceDigest: "terminal-evidence", recordedAt: 4,
  }))();
  database.transaction(() => rotations.appendInCurrentTransaction({
    runId: "run", agentId: "chair", custodyId: "custody",
    expectedRevision: head.revision, state: "committing",
    terminalEvidenceDigest: "terminal-evidence", recordedAt: 5,
  }))();
  database.prepare(`
    INSERT INTO review_terminal_sequence_high_water(run_id,terminal_sequence,revision)
    VALUES ('run',7,1)
  `).run();
  database.prepare(`
    INSERT INTO review_completion_targets(
      run_id,target_generation,preparation_id,review_subject_digest,task_id,
      reviewed_artifact_id,reviewed_artifact_revision,publication_lineage_digest,
      delivery_review_basis_revision,delivery_review_basis_digest,
      repository_source_state_digest,bundle_generation,bundle_digest,
      manifest_body_digest,manifest_root_digest,coverage_digest,
      bundle_search_index_digest,risk_read_map_digest,mandatory_read_set_digest,
      mandatory_read_count,mandatory_read_bytes,object_count,chunk_count,
      total_object_bytes,profile_id,profile_schema_digest,resolved_profile_digest,
      initial_chair_binding_digest,state,created_at
    ) VALUES (
      'run',3,'preparation','review-subject','task','artifact',1,'lineage',
      1,'review-basis','repository-source',1,'bundle','manifest-body','manifest-root',
      'coverage','search-index','risk-map','mandatory-set',0,0,0,0,0,
      'profile','profile-schema','profile-digest','initial-binding','current',1
    )
  `).run();
  database.prepare(`
    INSERT INTO review_target_chair_bindings(
      run_id,target_generation,binding_generation,predecessor_binding_generation,
      predecessor_binding_digest,predecessor_certification_cut_sequence,
      predecessor_certification_cut_digest,predecessor_certification_cut_custody_agent_id,
      predecessor_certification_cut_custody_id,
      predecessor_certification_cut_custody_revision,agent_id,principal_generation,
      chair_lease_generation,provider_session_generation,bridge_generation,adapter_id,
      adapter_contract_digest,model_family,model,review_subject_digest,
      route_receipt_digest,profile_digest,task_id,reviewed_artifact_id,
      delivery_review_basis_digest,repository_source_state_digest,bundle_digest,
      lifecycle_custody_id,lifecycle_custody_revision,checkpoint_digest,
      lifecycle_adoption_evidence_digest,binding_digest,created_at
    ) VALUES (
      'run',3,1,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'chair',1,1,1,1,
      'source-adapter','source-contract','claude','model','review-subject',
      'route-receipt','profile-digest','task','artifact','review-basis',
      'repository-source','bundle',NULL,NULL,NULL,NULL,'binding-one',1
    )
  `).run();
  database.prepare(`
    INSERT INTO review_target_chair_binding_heads(
      run_id,target_generation,active_binding_generation,revision
    ) VALUES ('run',3,1,1)
  `).run();
  seedLifecycleScope(database);
  database.pragma("foreign_keys = ON");
  expect(database.pragma("foreign_key_check")).toEqual([]);
  return database;
}

function prepareReview(database: Database.Database): Readonly<{
  store: LifecycleReviewAdoptionStore;
  context: PreparedReviewAdoptionContext;
}> {
  const store = new LifecycleReviewAdoptionStore(database);
  const plan = database.transaction(() => store.prepareReservationInCurrentTransaction({
    runId: "run",
    agentId: "chair",
    custodyId: "custody",
    applyId: "apply",
    commandId: "review-command",
    head: {
      projectSessionId: "session",
      runId: "run",
      agentId: "chair",
      custodyId: "custody",
      revision: 5,
      state: "committing",
      disposition: "none",
      semanticDigest: "committing-semantic",
      sourceRefDigest: "committing-source",
      journalDigest: "committing-journal",
      terminal: false,
    },
    finalRevision: 6,
    finalSourceRefDigest: "custody-final-source",
    lifecycleAdoptionEvidenceDigest: "adoption-evidence",
    recordedAt: 2,
    source: {
      source_adapter_id: "source-adapter",
      source_adapter_contract_digest: "source-contract",
      source_principal_generation: 1,
      source_provider_generation: 1,
      source_bridge_generation: 1,
      source_chair_lease_generation: 1,
      target_principal_generation: 2,
      target_provider_generation: 2,
      target_bridge_generation: 2,
      replacement_adapter_id: "replacement-adapter",
      replacement_contract_digest: "replacement-contract",
      checkpoint_digest: "checkpoint",
    },
    mutationPlan: { schemaVersion: 1, writes: ["review"], writeSetDigest: "write-set" },
  }))();
  const preparation = store.buildBatchPreparation({
    plan,
    projectSessionId: "session",
    runId: "run",
    agentId: "chair",
    ownerRef: finalizedCustodyOwnerRef(),
    ownerRefDigest: "owner-ref",
    finalRevision: 6,
    custodyTerminalSubjectDigest: lifecycleDigest("receipt-subject", {
      ownerRef: finalizedCustodyOwnerRef(),
    }),
    lifecycleAdoptionEvidenceDigest: "adoption-evidence",
    transitionReplayDigest: "transition-replay",
  });
  const prepared = preparation.prepared!;
  const intent = { ...prepared.pendingIntent, batchId: "batch" };
  const review = {
    reservationId: plan.reservationId,
    reservationDigest: plan.reservationDigest,
    decision: plan.decision,
    decisionDigest: plan.decisionDigest,
    cut: plan.cut,
    cutDigest: plan.cutDigest,
    subject: prepared.subject,
    subjectJson: prepared.subjectJson,
    subjectDigest: prepared.subjectDigest,
    intent,
    intentJson: canonicalJson(intent),
    intentDigest: lifecycleDigest("receipt-intent", intent),
    successorBinding: plan.successorBinding,
    rebindReceipt: plan.rebindReceipt,
  };
  return {
    store,
    context: {
      projectSessionId: "session",
      runId: "run",
      agentId: "chair",
      custodyId: "custody",
      finalRevision: 6,
      applyId: "apply",
      batchId: "batch",
      review,
    },
  };
}

function seedAuthorizedReviewApply(
  database: Database.Database,
  context: PreparedReviewAdoptionContext,
  options: Readonly<{ custodySubjectJson?: string }> = {},
): string {
  const review = context.review!;
  const receiptOne = "custody-authority-receipt";
  const receiptTwo = "review-authority-receipt";
  const effectDigest = "custody-effect";
  const effectsSetDigest = "effects-set";
  const completionDigest = "batch-completion";
  const receiptSetDigest = "receipt-set";
  const checkpoint = database.prepare(`
    SELECT checkpoint_digest FROM lifecycle_receipt_scope_checkpoints
     WHERE project_session_id='session' AND run_id='run' AND receipt_count=0
  `).get() as { checkpoint_digest: string };
  const before = database.prepare(`
    SELECT journal_digest FROM lifecycle_rotation_custody_revisions
     WHERE run_id='run' AND agent_id='chair' AND custody_id='custody' AND revision=5
  `).get() as { journal_digest: string };
  const finalBody = {
    schemaVersion: 1,
    custodyId: "custody",
    revision: 6,
    state: "finalized",
    disposition: "adopted",
    proofKind: "provider-terminal",
    terminalEvidenceDigest: "terminal-evidence",
  };
  const finalSemanticDigest = lifecycleDigest("custody-semantic", finalBody);
  const finalSourceRefDigest = finalSemanticDigest;
  const finalJournalDigest = lifecycleDigest("custody-journal", {
    revision: 6,
    priorJournalDigest: before.journal_digest,
    semanticDigest: finalSemanticDigest,
  });
  const applyDigest = "transition-apply-digest";
  const custodySubject = { ownerRef: finalizedCustodyOwnerRef() };
  const custodySubjectJson = canonicalJson(custodySubject);
  const custodySubjectDigest = lifecycleDigest("receipt-subject", custodySubject);

  database.transaction(() => {
    database.prepare(`
      INSERT INTO lifecycle_receipt_batches(
        batch_id,planned_apply_id,project_session_id,run_id,agent_id,transition_kind,
        planned_apply_kind,effects_set_digest,mutation_plan_digest,transition_replay_json,
        transition_replay_digest,ordered_subject_set_digest,receipt_intent_count,
        secondary_intent_kind,review_adoption_reservation_id,
        review_adoption_reservation_digest,review_decision_loss_effect_key,
        fresh_handoff_key,created_at
      ) VALUES ('batch','apply','session','run','chair','custody-terminal','terminal',
                ?,?,'{"terminalDisposition":"adopted"}','transition-replay',
                'subject-set',2,'review-adoption-decision',?,?,'none','none',6)
    `).run(effectsSetDigest, "write-set", review.reservationId, review.reservationDigest);
    database.prepare(`
      INSERT INTO lifecycle_receipt_custody_effects(
        batch_id,ordinal,role,transition_kind,planned_apply_id,project_session_id,
        run_id,agent_id,custody_id,pre_revision,pre_journal_digest,final_revision,
        final_semantic_digest,final_source_ref_digest,effect_digest
      ) VALUES ('batch',1,'primary','custody-terminal','apply','session','run','chair',
                'custody',5,?,6,?,?,?)
    `).run(before.journal_digest, finalSemanticDigest, finalSourceRefDigest, effectDigest);
    database.prepare(`
      INSERT INTO lifecycle_receipt_intents(
        batch_id,ordinal,batch_transition_kind,batch_intent_count,
        batch_secondary_intent_kind,kind,project_session_id,run_id,agent_id,
        subject_owner_kind,subject_owner_id,subject_owner_revision,custody_effect_digest,
        subject_json,subject_digest,intent_digest,created_at
      ) VALUES ('batch',1,'custody-terminal',2,'review-adoption-decision',
                'custody-terminal','session','run','chair','custody','custody',6,?,
                ?,?,'custody-intent',6)
    `).run(effectDigest, options.custodySubjectJson ?? custodySubjectJson, custodySubjectDigest);
    database.prepare(`
      INSERT INTO lifecycle_receipt_intents(
        batch_id,ordinal,batch_transition_kind,batch_intent_count,
        batch_secondary_intent_kind,kind,project_session_id,run_id,agent_id,
        subject_owner_kind,subject_owner_id,subject_owner_revision,custody_effect_digest,
        subject_json,subject_digest,intent_digest,created_at
      ) VALUES ('batch',2,'custody-terminal',2,'review-adoption-decision',
                'review-adoption-decision','session','run','chair','custody','custody',6,?,?,?,?,6)
    `).run(effectDigest, review.subjectJson, review.subjectDigest, review.intentDigest);
    database.prepare(`
      INSERT INTO lifecycle_authority_receipts(
        intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,kind,
        subject_owner_kind,subject_owner_id,subject_owner_revision,subject_digest,
        authority_id,authority_sequence,previous_authority_sequence,previous_receipt_digest,
        receipt_json,receipt_digest,attestation,verified_at
      ) VALUES ('custody-intent','batch',1,'session','run','chair','custody-terminal',
                'custody','custody',6,?,'authority',1,NULL,NULL,
                '{}',?,'attestation-one',6)
    `).run(custodySubjectDigest, receiptOne);
    database.prepare(`
      INSERT INTO lifecycle_authority_receipts(
        intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,kind,
        subject_owner_kind,subject_owner_id,subject_owner_revision,subject_digest,
        authority_id,authority_sequence,previous_authority_sequence,previous_receipt_digest,
        receipt_json,receipt_digest,attestation,verified_at
      ) VALUES (?,'batch',2,'session','run','chair','review-adoption-decision',
                'custody','custody',6,?,'authority',2,1,?,'{}',?,'attestation-two',6)
    `).run(review.intentDigest, review.subjectDigest, receiptOne, receiptTwo);
    database.prepare(`
      INSERT INTO lifecycle_receipt_batch_completions(
        batch_id,transition_kind,receipt_intent_count,secondary_intent_kind,
        ordinal_one,ordinal_one_intent_digest,ordinal_one_subject_digest,
        ordinal_one_receipt_digest,ordinal_two,ordinal_two_intent_digest,
        ordinal_two_subject_digest,ordinal_two_receipt_digest,effects_set_digest,
        primary_custody_effect_digest,ordered_authority_receipt_set_digest,
        completion_json,completion_digest,completed_at
      ) VALUES ('batch','custody-terminal',2,'review-adoption-decision',1,
                'custody-intent',?,?,2,?,?,?, ?,?,?, '{}',?,6)
    `).run(
      custodySubjectDigest, receiptOne, review.intentDigest, review.subjectDigest, receiptTwo,
      effectsSetDigest, effectDigest, receiptSetDigest, completionDigest,
    );
    database.prepare(`
      INSERT INTO lifecycle_receipt_batch_authorizations(
        batch_id,project_session_id,run_id,batch_completion_digest,
        ordered_authority_receipt_set_digest,verified_scope_checkpoint_digest,
        authorized_at,authorization_digest
      ) VALUES ('batch','session','run',?,?,?,?,? )
    `).run(completionDigest, receiptSetDigest, checkpoint.checkpoint_digest, 6, "authorization");
    database.prepare(`
      INSERT INTO lifecycle_rotation_custody_revisions(
        project_session_id,run_id,agent_id,custody_id,revision,prior_revision,
        prior_journal_digest,state,disposition_code,proof_kind,terminal_evidence_digest,
        semantic_json,semantic_digest,source_ref_digest,receipt_batch_id,
        receipt_apply_id,receipt_apply_digest,journal_json,journal_digest,recorded_at
      ) VALUES ('session','run','chair','custody',6,5,?,'finalized','adopted',
                'provider-terminal','terminal-evidence',?,?,?,?,?,?,'{}',?,6)
    `).run(
      before.journal_digest, canonicalJson(finalBody), finalSemanticDigest,
      finalSourceRefDigest, "batch", "apply", applyDigest, finalJournalDigest,
    );
    database.prepare(`
      UPDATE lifecycle_rotation_custody_heads
         SET current_revision=6,state='finalized',disposition_code='adopted',
             semantic_digest=?,source_ref_digest=?,journal_digest=?,terminal=1,
             head_revision=head_revision+1
       WHERE run_id='run' AND agent_id='chair' AND custody_id='custody'
         AND current_revision=5 AND terminal=0
    `).run(finalSemanticDigest, finalSourceRefDigest, finalJournalDigest);
    database.prepare(`
      INSERT INTO lifecycle_transition_applies(
        apply_id,apply_kind,batch_transition_kind,receipt_batch_id,
        batch_completion_digest,transition_replay_digest,
        ordered_authority_receipt_set_digest,verified_scope_checkpoint_digest,
        applied_mutation_plan_digest,fresh_handoff_key,fresh_generation_loss_after_key,
        local_write_set_digest,apply_json,apply_digest,applied_at
      ) VALUES ('apply','terminal','custody-terminal','batch',?,'transition-replay',
                ?,?,'write-set','none','none','local-write-set','{}',?,6)
    `).run(completionDigest, receiptSetDigest, checkpoint.checkpoint_digest, applyDigest);
  }).immediate();
  expect(database.pragma("foreign_key_check")).toEqual([]);
  return receiptTwo;
}

describe("lifecycle review adoption store", () => {
  it("binds the target snapshot, certification cut and rebind receipt into one atomic adoption", () => {
    const database = openReviewDatabase();
    try {
      const { store, context } = prepareReview(database);
      const review = context.review!;
      expect(review.decision).toMatchObject({
        outcome: "rebound",
        targetGeneration: 3,
        predecessorBindingGeneration: 1,
        predecessorBindingDigest: "binding-one",
        terminalSequenceHighWater: 7,
        target: { state: "current", review_subject_digest: "review-subject" },
        predecessorBinding: { binding_generation: 1, binding_digest: "binding-one" },
      });
      expect(review.cut).toMatchObject({
        runId: "run",
        targetGeneration: 3,
        predecessorBindingGeneration: 1,
        predecessorBindingDigest: "binding-one",
        terminalSequenceHighWater: 7,
        lifecycleCustodyRef: {
          runId: "run",
          agentId: "chair",
          custodyId: "custody",
          custodyRevision: 6,
        },
        lifecycleAdoptionEvidenceDigest: "adoption-evidence",
      });
      expect(review.successorBinding).toMatchObject({
        binding_generation: 2,
        predecessor_binding_generation: 1,
        lifecycle_custody_id: "custody",
        lifecycle_custody_revision: 6,
        principal_generation: 2,
        provider_session_generation: 2,
        bridge_generation: 2,
      });
      expect(review.rebindReceipt).toMatchObject({
        command_id: "review-command",
        prior_binding_generation: 1,
        new_binding_generation: 2,
        prior_binding_digest: "binding-one",
        new_binding_digest: review.successorBinding!.binding_digest,
      });

      const reviewReceiptDigest = seedAuthorizedReviewApply(database, context);
      database.transaction(() => store.writePostStateInCurrentTransaction(
        context,
        { receiptDigest: reviewReceiptDigest },
        7,
      ))();

      expect(database.prepare(`
        SELECT predecessor_binding_generation,terminal_sequence_high_water,
               lifecycle_custody_id,lifecycle_custody_revision,cut_digest
          FROM review_certification_cuts
      `).get()).toEqual({
        predecessor_binding_generation: 1,
        terminal_sequence_high_water: 7,
        lifecycle_custody_id: "custody",
        lifecycle_custody_revision: 6,
        cut_digest: review.cutDigest,
      });
      expect(database.prepare(`
        SELECT active_binding_generation,revision FROM review_target_chair_binding_heads
         WHERE run_id='run' AND target_generation=3
      `).get()).toEqual({ active_binding_generation: 2, revision: 2 });
      expect(database.prepare(`
        SELECT binding_generation,binding_digest FROM review_target_chair_bindings
         WHERE run_id='run' AND target_generation=3 ORDER BY binding_generation
      `).all()).toEqual([
        { binding_generation: 1, binding_digest: "binding-one" },
        { binding_generation: 2, binding_digest: review.successorBinding!.binding_digest },
      ]);
      expect(database.prepare(`
        SELECT command_id,rebind_receipt_digest FROM review_target_rebind_receipts
      `).get()).toEqual({
        command_id: "review-command",
        rebind_receipt_digest: review.rebindReceipt!.rebind_receipt_digest,
      });
      expect(database.prepare(`
        SELECT receipt_digest,review_decision_digest,certification_cut_digest
          FROM lifecycle_review_authority_bindings
      `).get()).toEqual({
        receipt_digest: reviewReceiptDigest,
        review_decision_digest: review.decisionDigest,
        certification_cut_digest: review.cutDigest,
      });
      expect(database.pragma("foreign_key_check")).toEqual([]);
    } finally {
      database.close();
    }
  });

  it("leaves a superseded target stale without a cut or rebind", () => {
    const database = openReviewDatabase();
    try {
      database.prepare(`
        UPDATE review_completion_targets SET state='superseded'
         WHERE run_id='run' AND target_generation=3
      `).run();
      const { store, context } = prepareReview(database);
      const review = context.review!;
      expect(review.decision).toMatchObject({
        outcome: "left-stale",
        reason: "no-current-target",
        targetGeneration: null,
        target: null,
        predecessorBinding: null,
        certificationCut: null,
        successorBinding: null,
      });
      expect(review.cut).toBeNull();
      expect(review.cutDigest).toBeNull();
      expect(review.successorBinding).toBeNull();
      expect(review.rebindReceipt).toBeNull();

      const receiptDigest = seedAuthorizedReviewApply(database, context);
      database.transaction(() => store.writePostStateInCurrentTransaction(
        context,
        { receiptDigest },
        7,
      ))();

      expect(database.prepare("SELECT count(*) AS count FROM review_certification_cuts").get())
        .toEqual({ count: 0 });
      expect(database.prepare("SELECT count(*) AS count FROM review_target_rebind_receipts").get())
        .toEqual({ count: 0 });
      expect(database.prepare(`
        SELECT active_binding_generation,revision FROM review_target_chair_binding_heads
         WHERE run_id='run' AND target_generation=3
      `).get()).toEqual({ active_binding_generation: 1, revision: 1 });
      expect(database.prepare(`
        SELECT review_decision_digest,certification_cut_digest
          FROM lifecycle_review_authority_bindings
      `).get()).toEqual({
        review_decision_digest: review.decisionDigest,
        certification_cut_digest: null,
      });
      expect(database.pragma("foreign_key_check")).toEqual([]);
    } finally {
      database.close();
    }
  });

  it("rejects a prepared review decision tampered after reservation", () => {
    const database = openReviewDatabase();
    try {
      const { store, context } = prepareReview(database);
      const review = context.review!;
      const tampered = {
        ...context,
        review: {
          ...review,
          decision: { ...review.decision, outcome: "left-stale" },
        },
      };

      expect(() => database.transaction(() => store.writePostStateInCurrentTransaction(
        tampered,
        { receiptDigest: "review-receipt" },
        3,
      ))()).toThrow("prepared review adoption is not authentic");
      expect(database.prepare("SELECT count(*) AS count FROM review_certification_cuts").get())
        .toEqual({ count: 0 });
      expect(database.prepare(`
        SELECT active_binding_generation,revision FROM review_target_chair_binding_heads
         WHERE run_id='run' AND target_generation=3
      `).get()).toEqual({ active_binding_generation: 1, revision: 1 });
    } finally {
      database.close();
    }
  });

  it("rejects a coherently resealed rebind receipt not covered by the authority receipt", () => {
    const database = openReviewDatabase();
    try {
      const { store, context } = prepareReview(database);
      const authorityReceiptDigest = seedAuthorizedReviewApply(database, context);
      const review = context.review!;
      const { rebind_receipt_digest: _digest, ...originalReceiptBody } = review.rebindReceipt!;
      const rebindReceiptBody = { ...originalReceiptBody, command_id: "forged-command" };
      const rebindReceipt = {
        ...rebindReceiptBody,
        rebind_receipt_digest: lifecycleDigest("review-target-rebind-receipt", rebindReceiptBody),
      };
      const subject = { ...review.subject, rebindReceipt };
      const subjectJson = canonicalJson(subject);
      const subjectDigest = lifecycleDigest("receipt-subject", subject);
      const intent = { ...review.intent, subjectDigest };
      const intentJson = canonicalJson(intent);
      const intentDigest = lifecycleDigest("receipt-intent", intent);
      const tampered = {
        ...context,
        review: {
          ...review,
          rebindReceipt,
          subject,
          subjectJson,
          subjectDigest,
          intent,
          intentJson,
          intentDigest,
        },
      };

      expect(() => database.transaction(() => store.writePostStateInCurrentTransaction(
        tampered,
        { receiptDigest: authorityReceiptDigest },
        7,
      ))()).toThrow("prepared review adoption is not authentic");
      expect(database.prepare("SELECT count(*) AS count FROM review_certification_cuts").get())
        .toEqual({ count: 0 });
      expect(database.prepare("SELECT count(*) AS count FROM review_target_rebind_receipts").get())
        .toEqual({ count: 0 });
      expect(database.prepare("SELECT count(*) AS count FROM lifecycle_review_authority_bindings").get())
        .toEqual({ count: 0 });
      expect(database.pragma("foreign_key_check")).toEqual([]);
    } finally {
      database.close();
    }
  });

  it("rejects a resealed and authorized subject that no longer matches its reservation", () => {
    const database = openReviewDatabase();
    try {
      const { store, context } = prepareReview(database);
      const review = context.review!;
      const subject = { ...review.subject, lifecycleAdoptionEvidenceDigest: "forged-evidence" };
      const subjectJson = canonicalJson(subject);
      const subjectDigest = lifecycleDigest("receipt-subject", subject);
      const intent = { ...review.intent, subjectDigest };
      const intentJson = canonicalJson(intent);
      const intentDigest = lifecycleDigest("receipt-intent", intent);
      const tampered = {
        ...context,
        review: { ...review, subject, subjectJson, subjectDigest, intent, intentJson, intentDigest },
      };
      const receiptDigest = seedAuthorizedReviewApply(database, tampered);

      expect(() => database.transaction(() => store.writePostStateInCurrentTransaction(
        tampered,
        { receiptDigest },
        7,
      ))()).toThrow("prepared review adoption is not authentic");
      expect(database.prepare("SELECT count(*) AS count FROM review_certification_cuts").get())
        .toEqual({ count: 0 });
      expect(database.prepare("SELECT count(*) AS count FROM review_target_rebind_receipts").get())
        .toEqual({ count: 0 });
      expect(database.prepare("SELECT count(*) AS count FROM lifecycle_review_authority_bindings").get())
        .toEqual({ count: 0 });
      expect(database.pragma("foreign_key_check")).toEqual([]);
    } finally {
      database.close();
    }
  });

  it("rejects a secondary subject anchored to inconsistent primary subject JSON", () => {
    const database = openReviewDatabase();
    try {
      const { store, context } = prepareReview(database);
      const receiptDigest = seedAuthorizedReviewApply(database, context, {
        custodySubjectJson: canonicalJson({
          ownerRef: finalizedCustodyOwnerRef(),
          forged: true,
        }),
      });

      expect(() => database.transaction(() => store.writePostStateInCurrentTransaction(
        context,
        { receiptDigest },
        7,
      ))()).toThrow("prepared review adoption is not authentic");
      expect(database.prepare("SELECT count(*) AS count FROM review_certification_cuts").get())
        .toEqual({ count: 0 });
      expect(database.prepare("SELECT count(*) AS count FROM review_target_rebind_receipts").get())
        .toEqual({ count: 0 });
      expect(database.prepare("SELECT count(*) AS count FROM lifecycle_review_authority_bindings").get())
        .toEqual({ count: 0 });
      expect(database.pragma("foreign_key_check")).toEqual([]);
    } finally {
      database.close();
    }
  });

  it("persists adoption authority and its cut without rebinding a target that became stale", () => {
    const database = openReviewDatabase();
    try {
      const { store, context } = prepareReview(database);
      const receiptDigest = seedAuthorizedReviewApply(database, context);
      database.prepare(`
        UPDATE review_completion_targets SET state='superseded'
         WHERE run_id='run' AND target_generation=3
      `).run();

      database.transaction(() => store.writePostStateInCurrentTransaction(
        context,
        { receiptDigest },
        7,
      ))();
      expect(database.prepare("SELECT count(*) AS count FROM review_certification_cuts").get())
        .toEqual({ count: 1 });
      expect(database.prepare("SELECT count(*) AS count FROM review_target_rebind_receipts").get())
        .toEqual({ count: 0 });
      expect(database.prepare(`
        SELECT active_binding_generation,revision FROM review_target_chair_binding_heads
         WHERE run_id='run' AND target_generation=3
      `).get()).toEqual({ active_binding_generation: 1, revision: 1 });
      expect(database.prepare("SELECT count(*) AS count FROM lifecycle_review_authority_bindings").get())
        .toEqual({ count: 1 });
      expect(database.pragma("foreign_key_check")).toEqual([]);
    } finally {
      database.close();
    }
  });

  it("rejects every transactional write entry point before reading or writing the database", () => {
    const database = new Database(":memory:");
    try {
      const store = new LifecycleReviewAdoptionStore(database);
      const context = {
        projectSessionId: "project-session-01",
        runId: "run-01",
        agentId: "agent-01",
        custodyId: "custody-01",
        finalRevision: 2,
        applyId: "apply-01",
        batchId: "batch-01",
        review: null,
      } as const;
      const calls = [
        () => store.persistPreparedIntentInCurrentTransaction({
          preparation: {
            prepared: null,
            intentCount: 1,
            intentCountDec: "1",
            secondaryKind: "none",
            reservationId: null,
            reservationDigest: null,
            reservationRef: null,
            orderedSubjectMembers: [],
          },
          ...context,
          custodyEffectDigest: "sha256:effect",
          recordedAt: 1,
        }),
        () => store.persistAuthorityReceiptInCurrentTransaction(context, null),
        () => store.prepareReservationInCurrentTransaction({
          runId: context.runId,
          agentId: context.agentId,
          custodyId: context.custodyId,
          applyId: context.applyId,
          commandId: "command-01",
          head: {
            ...context,
            revision: 1,
            state: "committing",
            disposition: "none",
            semanticDigest: "sha256:semantic",
            sourceRefDigest: "sha256:source",
            journalDigest: "sha256:journal",
            terminal: false,
          },
          finalRevision: context.finalRevision,
          finalSourceRefDigest: "sha256:final-source",
          lifecycleAdoptionEvidenceDigest: "sha256:adoption",
          recordedAt: 1,
          source: {},
          mutationPlan: { schemaVersion: 1, writes: [], writeSetDigest: "sha256:writes" },
        }),
        () => store.writePostStateInCurrentTransaction(
          context,
          { receiptDigest: "sha256:receipt" },
          1,
        ),
      ];

      const errors = calls.map((call) => {
        try {
          call();
          return null;
        } catch (error: unknown) {
          return error instanceof Error ? error.message : String(error);
        }
      });

      expect(errors).toEqual(Array(4).fill("lifecycle review adoption requires a transaction"));
      expect(database.prepare("SELECT total_changes() AS count").get()).toEqual({ count: 0 });
    } finally {
      database.close();
    }
  });
});
