# Agent Fabric retention, receipts, and exports

## Eligible delivery-source schema

~~~sql
delivery_run_starts(
  project_session_id, run_id, delivery_run_id, repository_object_format,
  approved_base_object_id, authority_digest, created_revision,
  PRIMARY KEY(project_session_id, run_id, delivery_run_id)
)

delivery_requirement_maps(
  run_id, delivery_run_id, map_generation, closure_digest, catalogue_digest,
  accepted_scope_artifact_id, accepted_scope_revision,
  accepted_scope_digest, source_set_digest, requirement_set_digest,
  artifact_id, artifact_revision, content_digest, current, private_cas_path,
  PRIMARY KEY(run_id, delivery_run_id, map_generation),
  UNIQUE(content_digest)
)

coordination_gate_snapshots(
  run_id, delivery_run_id, snapshot_generation, event_watermark,
  chair_snapshot_digest, authority_digest, accepted_scope_digest,
  requirement_map_digest, gate_closure_digest, objective_evidence_digest,
  artifact_id, artifact_revision, content_digest, private_cas_path,
  PRIMARY KEY(run_id, delivery_run_id, snapshot_generation),
  UNIQUE(content_digest)
)

implementation_delivery_manifests(
  run_id, delivery_run_id, seal_generation, command_id,
  snapshot_generation, profile_digest, accepted_scope_digest,
  requirement_map_digest, evidence_closure_digest,
  base_object_id, head_object_id, head_tree_id, index_tree_id,
  repository_source_state_digest, artifact_id, artifact_revision,
  content_digest, publication_lineage_digest, private_cas_path,
  PRIMARY KEY(run_id, delivery_run_id, seal_generation),
  UNIQUE(content_digest)
)

delivery_review_bases(
  run_id, delivery_run_id, review_basis_revision,
  manifest_artifact_id, manifest_artifact_revision, manifest_digest,
  snapshot_digest, profile_digest, repository_source_state_digest,
  requirement_map_digest, evidence_closure_digest, current, basis_digest,
  PRIMARY KEY(run_id, delivery_run_id, review_basis_revision)
)
~~~

## Run plan declaration retention

~~~sql
run_plan_declarations(
  run_id, plan_revision,
  plan_path, plan_digest,
  accepted_scope_artifact_id, accepted_scope_revision,
  accepted_scope_path, accepted_scope_digest,
  declared_task_denominator NULL-or-positive,
  declared_by_agent_id, declared_at,
  PRIMARY KEY(run_id, plan_revision),
  FOREIGN KEY(accepted_scope_artifact_id) REFERENCES artifacts(artifact_id),
  FOREIGN KEY(run_id, declared_by_agent_id) REFERENCES agents(run_id, agent_id)
)
~~~

Rows are immutable and retained with the coordination run. INSERT requires the
next contiguous per-run `plan_revision`; UPDATE and DELETE abort. The accepted
scope path/digest/revision are equality-copied from the active registered scope
at declaration time so later scope change cannot rewrite historical plan
meaning. Each append advances both the global projection revision and the run
item revision. Current plan and finite-progress projection select only the
highest stored revision; retention keeps every declaration in revision order.

## Receipt effects, authorisation, and transition application

~~~sql
lifecycle_receipt_fresh_origin_effects(
  batch_id, ordinal CHECK(ordinal IN (1,2)),
  role CHECK(role IN ('primary','secondary')),
  transition_kind CHECK(transition_kind IN ('custody-terminal','fresh-origin')),
  batch_intent_count, batch_secondary_intent_kind,
  planned_apply_id, project_session_id, run_id, agent_id,
  handoff_id, handoff_digest, source_mode CHECK(source_mode IN
    ('terminalize-nonfinal-custody','reuse-final-custody',
      'open-generation-loss')),
  recovery_source_kind, recovery_source_ref_digest, source_journal_digest,
  admission_digest, fresh_apply_plan_digest,
  new_custody_id, new_custody_revision CHECK(new_custody_revision=1),
  new_custody_semantic_digest, new_custody_source_ref_digest,
  affected_generation_loss_id, affected_generation_loss_before_revision,
  affected_generation_loss_before_state,
  affected_generation_loss_before_source_ref_digest,
  affected_generation_loss_before_journal_digest,
  affected_generation_loss_after_revision,
  affected_generation_loss_after_semantic_digest,
  affected_generation_loss_after_source_ref_digest,
  affected_generation_loss_after_key NOT NULL,
  effect_digest UNIQUE,
  PRIMARY KEY(batch_id,ordinal),
  UNIQUE(batch_id,ordinal,role,effect_digest),
  UNIQUE(batch_id,effect_digest,project_session_id,run_id,agent_id,
    new_custody_id,new_custody_revision),
  UNIQUE(batch_id,ordinal,effect_digest,project_session_id,run_id,agent_id,
    new_custody_id,new_custody_revision),
  FOREIGN KEY(batch_id,transition_kind,batch_intent_count,
      batch_secondary_intent_kind)
    REFERENCES lifecycle_receipt_batches(
      batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  FOREIGN KEY(handoff_id,handoff_digest,planned_apply_id,project_session_id,
      run_id,agent_id,source_mode,recovery_source_kind,
      recovery_source_ref_digest,source_journal_digest,new_custody_id,
      new_custody_semantic_digest,new_custody_source_ref_digest,
      affected_generation_loss_after_key,admission_digest,
      fresh_apply_plan_digest)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
      agent_id,source_mode,recovery_source_kind,
      recovery_source_ref_digest,source_journal_digest,new_custody_id,
      new_custody_semantic_digest,new_custody_source_ref_digest,
      affected_generation_loss_after_key,admission_digest,
      fresh_apply_plan_digest),
  FOREIGN KEY(handoff_id,handoff_digest,affected_generation_loss_id,
      affected_generation_loss_before_revision,
      affected_generation_loss_before_state,
      affected_generation_loss_before_source_ref_digest,
      affected_generation_loss_before_journal_digest,
      affected_generation_loss_after_revision,
      affected_generation_loss_after_semantic_digest,
      affected_generation_loss_after_source_ref_digest)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,affected_generation_loss_id,
      affected_generation_loss_before_revision,
      affected_generation_loss_before_state,
      affected_generation_loss_before_source_ref_digest,
      affected_generation_loss_before_journal_digest,
      affected_generation_loss_after_revision,
      affected_generation_loss_after_semantic_digest,
      affected_generation_loss_after_source_ref_digest),
  CHECK((transition_kind='fresh-origin' AND ordinal=1 AND role='primary' AND
      batch_intent_count=1 AND batch_secondary_intent_kind='none' AND
      source_mode IN ('reuse-final-custody','open-generation-loss')) OR
    (transition_kind='custody-terminal' AND ordinal=2 AND role='secondary' AND
      batch_intent_count=2 AND batch_secondary_intent_kind='fresh-origin' AND
      source_mode='terminalize-nonfinal-custody')),
  CHECK((source_mode='reuse-final-custody' AND
      recovery_source_kind='custody' AND
      affected_generation_loss_after_key='none') OR
    (source_mode='open-generation-loss' AND
      recovery_source_kind='generation-loss' AND
      affected_generation_loss_id IS NOT NULL AND
      affected_generation_loss_before_source_ref_digest=
        recovery_source_ref_digest AND
      affected_generation_loss_before_journal_digest=
        source_journal_digest) OR
    (source_mode='terminalize-nonfinal-custody' AND
      recovery_source_kind='custody')),
  CHECK((affected_generation_loss_after_key='none' AND
      affected_generation_loss_id IS NULL AND
      affected_generation_loss_before_revision IS NULL AND
      affected_generation_loss_before_state IS NULL AND
      affected_generation_loss_before_source_ref_digest IS NULL AND
      affected_generation_loss_before_journal_digest IS NULL AND
      affected_generation_loss_after_revision IS NULL AND
      affected_generation_loss_after_semantic_digest IS NULL AND
      affected_generation_loss_after_source_ref_digest IS NULL) OR
    (affected_generation_loss_after_key<>'none' AND
      affected_generation_loss_id IS NOT NULL AND
      affected_generation_loss_before_revision IS NOT NULL AND
      affected_generation_loss_before_state IS NOT NULL AND
      affected_generation_loss_before_source_ref_digest IS NOT NULL AND
      affected_generation_loss_before_journal_digest IS NOT NULL AND
      affected_generation_loss_after_revision=
        affected_generation_loss_before_revision+1 AND
      affected_generation_loss_after_semantic_digest IS NOT NULL AND
      affected_generation_loss_after_source_ref_digest=
        affected_generation_loss_after_key))
)

-- The shipped effect relation uses its two composite foreign keys to bind the
-- effect to the exact handoff identity and affected-loss tuple. Those foreign
-- keys, together with the arm checks above, reject missing or crossed handoffs;
-- there is no separate handoff-validation trigger.

lifecycle_receipt_intents(
  batch_id, ordinal CHECK(ordinal IN (1,2)),
  batch_transition_kind, batch_intent_count, batch_secondary_intent_kind,
  kind CHECK(kind IN ('custody-terminal','generation-loss-terminal',
    'custody-recovery-retirement','fresh-origin','review-adoption-decision')),
  project_session_id, run_id, agent_id,
  subject_owner_kind CHECK(subject_owner_kind IN
    ('custody','generation-loss','recovery-retirement')),
  subject_owner_id, subject_owner_revision CHECK(subject_owner_revision >= 1),
  custody_effect_digest, generation_loss_effect_role,
  generation_loss_effect_digest, recovery_retirement_effect_digest,
  fresh_origin_effect_digest,
  subject_json, subject_digest, intent_digest, created_at,
  PRIMARY KEY(batch_id,ordinal), UNIQUE(intent_digest),
  UNIQUE(intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,
    kind,subject_owner_kind,subject_owner_id,subject_owner_revision,
    subject_digest),
  UNIQUE(kind,project_session_id,run_id,agent_id,subject_owner_kind,
    subject_owner_id,subject_owner_revision),
  FOREIGN KEY(batch_id,project_session_id,run_id,agent_id)
    REFERENCES lifecycle_receipt_batches(
      batch_id,project_session_id,run_id,agent_id),
  FOREIGN KEY(batch_id,batch_transition_kind,batch_intent_count)
    REFERENCES lifecycle_receipt_batches(
      batch_id,transition_kind,receipt_intent_count),
  FOREIGN KEY(batch_id,batch_transition_kind,batch_intent_count,
      batch_secondary_intent_kind)
    REFERENCES lifecycle_receipt_batches(
      batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  FOREIGN KEY(batch_id,custody_effect_digest,project_session_id,run_id,
      agent_id,subject_owner_id,subject_owner_revision)
    REFERENCES lifecycle_receipt_custody_effects(
      batch_id,effect_digest,project_session_id,run_id,agent_id,custody_id,
      final_revision),
  FOREIGN KEY(batch_id,generation_loss_effect_role,
      generation_loss_effect_digest,project_session_id,run_id,agent_id,
      subject_owner_id,subject_owner_revision)
    REFERENCES lifecycle_receipt_generation_loss_effects(
      batch_id,role,effect_digest,project_session_id,run_id,agent_id,
      generation_loss_id,final_revision),
  FOREIGN KEY(batch_id,recovery_retirement_effect_digest,project_session_id,
      run_id,agent_id,subject_owner_id,subject_owner_revision)
    REFERENCES lifecycle_receipt_recovery_retirement_effects(
      batch_id,effect_digest,project_session_id,run_id,agent_id,retirement_id,
      retirement_revision),
  FOREIGN KEY(batch_id,ordinal,fresh_origin_effect_digest,project_session_id,
      run_id,agent_id,subject_owner_id,subject_owner_revision)
    REFERENCES lifecycle_receipt_fresh_origin_effects(
      batch_id,ordinal,effect_digest,project_session_id,run_id,agent_id,
      new_custody_id,new_custody_revision),
  CHECK((ordinal=1 AND kind=batch_transition_kind) OR
    (ordinal=2 AND batch_transition_kind='custody-terminal' AND
      batch_intent_count=2 AND
      kind=batch_secondary_intent_kind AND
      kind IN ('fresh-origin','review-adoption-decision'))),
  CHECK(
    (kind IN ('custody-terminal','review-adoption-decision') AND
      subject_owner_kind='custody' AND custody_effect_digest IS NOT NULL AND
      generation_loss_effect_role IS NULL AND
      generation_loss_effect_digest IS NULL AND
      recovery_retirement_effect_digest IS NULL AND
      fresh_origin_effect_digest IS NULL) OR
    (kind='generation-loss-terminal' AND
      subject_owner_kind='generation-loss' AND
      custody_effect_digest IS NULL AND
      generation_loss_effect_role='primary' AND
      generation_loss_effect_digest IS NOT NULL AND
      recovery_retirement_effect_digest IS NULL AND
      fresh_origin_effect_digest IS NULL) OR
    (kind='custody-recovery-retirement' AND
      subject_owner_kind='recovery-retirement' AND
      custody_effect_digest IS NULL AND
      generation_loss_effect_role IS NULL AND
      generation_loss_effect_digest IS NULL AND
      recovery_retirement_effect_digest IS NOT NULL AND
      fresh_origin_effect_digest IS NULL) OR
    (kind='fresh-origin' AND subject_owner_kind='custody' AND
      custody_effect_digest IS NULL AND generation_loss_effect_role IS NULL AND
      generation_loss_effect_digest IS NULL AND
      recovery_retirement_effect_digest IS NULL AND
      fresh_origin_effect_digest IS NOT NULL))
)

lifecycle_authority_receipts(
  intent_digest PRIMARY KEY, batch_id, ordinal,
  project_session_id, run_id, agent_id, kind, subject_owner_kind,
  subject_owner_id, subject_owner_revision, subject_digest,
  authority_id, authority_sequence CHECK(authority_sequence >= 1),
  previous_authority_sequence, previous_receipt_digest,
  receipt_json, receipt_digest UNIQUE, attestation, verified_at,
  UNIQUE(project_session_id,run_id,authority_id,authority_sequence),
  UNIQUE(project_session_id,run_id,authority_id,authority_sequence,
    receipt_digest),
  UNIQUE(batch_id,ordinal,intent_digest,subject_digest,receipt_digest),
  UNIQUE(receipt_digest,kind,project_session_id,run_id,agent_id,
    subject_owner_kind,subject_owner_id,subject_owner_revision),
  UNIQUE(receipt_digest,intent_digest,batch_id,ordinal,kind,project_session_id,
    run_id,agent_id,subject_owner_kind,subject_owner_id,subject_owner_revision,
    subject_digest),
  UNIQUE(kind,project_session_id,run_id,agent_id,subject_owner_kind,
    subject_owner_id,subject_owner_revision),
  FOREIGN KEY(intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,
      kind,subject_owner_kind,subject_owner_id,subject_owner_revision,
      subject_digest)
    REFERENCES lifecycle_receipt_intents(
      intent_digest,batch_id,ordinal,project_session_id,run_id,agent_id,kind,
      subject_owner_kind,subject_owner_id,subject_owner_revision,subject_digest),
  FOREIGN KEY(project_session_id,run_id,authority_id)
    REFERENCES lifecycle_admitted_run_scopes(
      project_session_id,run_id,authority_id),
  FOREIGN KEY(project_session_id,run_id,authority_id,
      previous_authority_sequence,previous_receipt_digest)
    REFERENCES lifecycle_authority_receipts(
      project_session_id,run_id,authority_id,authority_sequence,receipt_digest),
  CHECK((authority_sequence=1 AND previous_authority_sequence IS NULL AND
      previous_receipt_digest IS NULL) OR
    (authority_sequence>1 AND
      previous_authority_sequence=authority_sequence-1 AND
      previous_receipt_digest IS NOT NULL))
)

lifecycle_receipt_batch_completions(
  batch_id PRIMARY KEY, transition_kind, receipt_intent_count,
  secondary_intent_kind,
  ordinal_one CHECK(ordinal_one=1), ordinal_one_intent_digest,
  ordinal_one_subject_digest,
  ordinal_one_receipt_digest,
  ordinal_two CHECK(ordinal_two IS NULL OR ordinal_two=2),
  ordinal_two_intent_digest, ordinal_two_subject_digest,
  ordinal_two_receipt_digest,
  primary_custody_effect_digest,
  primary_loss_effect_role CHECK(
    primary_loss_effect_role IS NULL OR primary_loss_effect_role='primary'),
  primary_loss_effect_digest, primary_retirement_effect_digest,
  linked_loss_effect_role CHECK(
    linked_loss_effect_role IS NULL OR linked_loss_effect_role='linked'),
  linked_loss_effect_digest,
  primary_fresh_effect_ordinal, primary_fresh_effect_role,
  primary_fresh_effect_digest,
  secondary_fresh_effect_ordinal, secondary_fresh_effect_role,
  secondary_fresh_effect_digest,
  ordered_authority_receipt_set_digest,
  completion_json, completion_digest UNIQUE, completed_at,
  UNIQUE(batch_id,completion_digest,ordered_authority_receipt_set_digest),
  FOREIGN KEY(batch_id,transition_kind,receipt_intent_count,
      secondary_intent_kind)
    REFERENCES lifecycle_receipt_batches(
      batch_id,transition_kind,receipt_intent_count,secondary_intent_kind),
  FOREIGN KEY(batch_id,ordinal_one,ordinal_one_intent_digest,
      ordinal_one_subject_digest,ordinal_one_receipt_digest)
    REFERENCES lifecycle_authority_receipts(
      batch_id,ordinal,intent_digest,subject_digest,receipt_digest),
  FOREIGN KEY(batch_id,ordinal_two,ordinal_two_intent_digest,
      ordinal_two_subject_digest,ordinal_two_receipt_digest)
    REFERENCES lifecycle_authority_receipts(
      batch_id,ordinal,intent_digest,subject_digest,receipt_digest),
  FOREIGN KEY(batch_id,primary_custody_effect_digest)
    REFERENCES lifecycle_receipt_custody_effects(batch_id,effect_digest),
  FOREIGN KEY(batch_id,primary_loss_effect_role,primary_loss_effect_digest)
    REFERENCES lifecycle_receipt_generation_loss_effects(
      batch_id,role,effect_digest),
  FOREIGN KEY(batch_id,primary_retirement_effect_digest)
    REFERENCES lifecycle_receipt_recovery_retirement_effects(
      batch_id,effect_digest),
  FOREIGN KEY(batch_id,linked_loss_effect_role,linked_loss_effect_digest)
    REFERENCES lifecycle_receipt_generation_loss_effects(
      batch_id,role,effect_digest),
  FOREIGN KEY(batch_id,primary_fresh_effect_ordinal,
      primary_fresh_effect_role,primary_fresh_effect_digest)
    REFERENCES lifecycle_receipt_fresh_origin_effects(
      batch_id,ordinal,role,effect_digest),
  FOREIGN KEY(batch_id,secondary_fresh_effect_ordinal,
      secondary_fresh_effect_role,secondary_fresh_effect_digest)
    REFERENCES lifecycle_receipt_fresh_origin_effects(
      batch_id,ordinal,role,effect_digest),
  CHECK((secondary_intent_kind='none' AND receipt_intent_count=1 AND
      ordinal_two IS NULL AND
      ordinal_two_intent_digest IS NULL AND
      ordinal_two_subject_digest IS NULL AND ordinal_two_receipt_digest IS NULL) OR
    (secondary_intent_kind<>'none' AND receipt_intent_count=2 AND ordinal_two=2 AND
      ordinal_two_intent_digest IS NOT NULL AND
      ordinal_two_subject_digest IS NOT NULL AND
      ordinal_two_receipt_digest IS NOT NULL)),
  CHECK((linked_loss_effect_role IS NULL)=(linked_loss_effect_digest IS NULL)),
  CHECK((transition_kind='custody-terminal' AND
      primary_custody_effect_digest IS NOT NULL AND
      primary_loss_effect_role IS NULL AND primary_loss_effect_digest IS NULL AND
      primary_retirement_effect_digest IS NULL AND
      primary_fresh_effect_ordinal IS NULL AND
      primary_fresh_effect_role IS NULL AND primary_fresh_effect_digest IS NULL AND
      ((secondary_intent_kind='fresh-origin' AND
          secondary_fresh_effect_ordinal=2 AND
          secondary_fresh_effect_role='secondary' AND
          secondary_fresh_effect_digest IS NOT NULL) OR
        (secondary_intent_kind<>'fresh-origin' AND
          secondary_fresh_effect_ordinal IS NULL AND
          secondary_fresh_effect_role IS NULL AND
          secondary_fresh_effect_digest IS NULL))) OR
    (transition_kind='generation-loss-terminal' AND
      primary_custody_effect_digest IS NULL AND
      primary_loss_effect_role='primary' AND
      primary_loss_effect_digest IS NOT NULL AND
      primary_retirement_effect_digest IS NULL AND
      linked_loss_effect_digest IS NULL AND
      primary_fresh_effect_ordinal IS NULL AND
      primary_fresh_effect_role IS NULL AND primary_fresh_effect_digest IS NULL AND
      secondary_fresh_effect_ordinal IS NULL AND
      secondary_fresh_effect_role IS NULL AND
      secondary_fresh_effect_digest IS NULL) OR
    (transition_kind='custody-recovery-retirement' AND
      primary_custody_effect_digest IS NULL AND
      primary_loss_effect_role IS NULL AND primary_loss_effect_digest IS NULL AND
      primary_retirement_effect_digest IS NOT NULL AND
      linked_loss_effect_digest IS NULL AND
      primary_fresh_effect_ordinal IS NULL AND
      primary_fresh_effect_role IS NULL AND primary_fresh_effect_digest IS NULL AND
      secondary_fresh_effect_ordinal IS NULL AND
      secondary_fresh_effect_role IS NULL AND
      secondary_fresh_effect_digest IS NULL) OR
    (transition_kind='fresh-origin' AND
      primary_custody_effect_digest IS NULL AND
      primary_loss_effect_role IS NULL AND primary_loss_effect_digest IS NULL AND
      primary_retirement_effect_digest IS NULL AND
      linked_loss_effect_digest IS NULL AND
      primary_fresh_effect_ordinal=1 AND
      primary_fresh_effect_role='primary' AND
      primary_fresh_effect_digest IS NOT NULL AND
      secondary_fresh_effect_ordinal IS NULL AND
      secondary_fresh_effect_role IS NULL AND
      secondary_fresh_effect_digest IS NULL))
)

lifecycle_review_authority_bindings(
  receipt_digest PRIMARY KEY, intent_digest UNIQUE, batch_id UNIQUE,
  ordinal CHECK(ordinal=2), subject_digest,
  kind CHECK(kind='review-adoption-decision'),
  subject_owner_kind CHECK(subject_owner_kind='custody'),
  project_session_id, run_id, agent_id, custody_id, custody_revision,
  review_reservation_digest, review_decision_digest,
  certification_cut_digest, certification_cut_key,
  decision_loss_after_id, decision_loss_after_revision,
  decision_loss_after_semantic_digest, decision_loss_after_source_ref_digest,
  decision_loss_after_key, decision_loss_effect_key NOT NULL,
  decision_loss_effect_role, decision_loss_effect_digest,
  apply_id UNIQUE,
  UNIQUE(receipt_digest,run_id,agent_id,custody_id,custody_revision,
    review_decision_digest,certification_cut_digest),
  FOREIGN KEY(receipt_digest,intent_digest,batch_id,ordinal,kind,
      project_session_id,run_id,agent_id,subject_owner_kind,custody_id,
      custody_revision,subject_digest)
    REFERENCES lifecycle_authority_receipts(
      receipt_digest,intent_digest,batch_id,ordinal,kind,project_session_id,run_id,
      agent_id,subject_owner_kind,subject_owner_id,subject_owner_revision,
      subject_digest),
  FOREIGN KEY(batch_id,apply_id,project_session_id,run_id,agent_id,
      review_reservation_digest)
    REFERENCES lifecycle_receipt_batches(
      batch_id,planned_apply_id,project_session_id,run_id,agent_id,
      review_adoption_reservation_digest),
  FOREIGN KEY(run_id,agent_id,custody_id,custody_revision)
    REFERENCES lifecycle_rotation_custody_revisions(
      run_id,agent_id,custody_id,revision),
  FOREIGN KEY(review_reservation_digest,project_session_id,run_id,agent_id,
      custody_id,custody_revision,review_decision_digest,certification_cut_key,
      decision_loss_after_key)
    REFERENCES lifecycle_review_adoption_reservations(
      reservation_digest,project_session_id,run_id,agent_id,custody_id,
      finalized_custody_revision,review_decision_digest,certification_cut_key,
      decision_loss_after_key),
  FOREIGN KEY(review_reservation_digest,decision_loss_after_id,
      decision_loss_after_revision,decision_loss_after_semantic_digest,
      decision_loss_after_source_ref_digest)
    REFERENCES lifecycle_review_adoption_reservations(
      reservation_digest,decision_loss_after_id,decision_loss_after_revision,
      decision_loss_after_semantic_digest,decision_loss_after_source_ref_digest),
  FOREIGN KEY(batch_id,decision_loss_effect_key)
    REFERENCES lifecycle_receipt_batches(
      batch_id,review_decision_loss_effect_key),
  FOREIGN KEY(batch_id,decision_loss_effect_key,
      decision_loss_effect_role,decision_loss_effect_digest,
      project_session_id,run_id,agent_id,decision_loss_after_id,
      decision_loss_after_revision,decision_loss_after_semantic_digest,
      decision_loss_after_source_ref_digest)
    REFERENCES lifecycle_receipt_batches(
      batch_id,review_decision_loss_effect_key,
      review_decision_loss_effect_role,review_decision_loss_effect_digest,
      project_session_id,run_id,agent_id,review_decision_loss_after_id,
      review_decision_loss_after_revision,
      review_decision_loss_after_semantic_digest,
      review_decision_loss_after_source_ref_digest),
  FOREIGN KEY(apply_id,batch_id)
    REFERENCES lifecycle_transition_applies(apply_id,receipt_batch_id)
    DEFERRABLE INITIALLY DEFERRED,
  CHECK(certification_cut_key IS NOT NULL AND
    decision_loss_after_key IS NOT NULL),
  CHECK((certification_cut_digest IS NULL AND certification_cut_key='none') OR
    (certification_cut_digest IS NOT NULL AND
      certification_cut_key=certification_cut_digest)),
  CHECK((decision_loss_after_key='none' AND decision_loss_after_id IS NULL AND
      decision_loss_after_revision IS NULL AND
      decision_loss_after_semantic_digest IS NULL AND
      decision_loss_after_source_ref_digest IS NULL) OR
    (decision_loss_after_key<>'none' AND decision_loss_after_id IS NOT NULL AND
      decision_loss_after_revision IS NOT NULL AND
      decision_loss_after_semantic_digest IS NOT NULL AND
      decision_loss_after_source_ref_digest=decision_loss_after_key)),
  CHECK((decision_loss_effect_key='none' AND
      decision_loss_effect_role IS NULL AND
      decision_loss_effect_digest IS NULL) OR
    (decision_loss_effect_key<>'none' AND
      decision_loss_effect_role='linked' AND
      decision_loss_effect_digest IS NOT NULL AND
      decision_loss_effect_digest=decision_loss_effect_key))
)

lifecycle_receipt_batch_authorizations(
  batch_id PRIMARY KEY, project_session_id, run_id, batch_completion_digest,
  ordered_authority_receipt_set_digest, verified_scope_checkpoint_digest,
  authorized_at, authorization_digest UNIQUE,
  UNIQUE(batch_id,verified_scope_checkpoint_digest),
  UNIQUE(batch_id,ordered_authority_receipt_set_digest,
    verified_scope_checkpoint_digest),
  UNIQUE(batch_id,batch_completion_digest,
    ordered_authority_receipt_set_digest,verified_scope_checkpoint_digest),
  FOREIGN KEY(batch_id,project_session_id,run_id)
    REFERENCES lifecycle_receipt_batches(batch_id,project_session_id,run_id),
  FOREIGN KEY(batch_id,batch_completion_digest,
      ordered_authority_receipt_set_digest)
    REFERENCES lifecycle_receipt_batch_completions(
      batch_id,completion_digest,ordered_authority_receipt_set_digest),
  FOREIGN KEY(project_session_id,run_id,verified_scope_checkpoint_digest)
    REFERENCES lifecycle_receipt_scope_checkpoints(
      project_session_id,run_id,checkpoint_digest)
)

lifecycle_transition_applies(
  apply_id PRIMARY KEY,
  apply_kind CHECK(apply_kind IN ('terminal','terminal-fresh','fresh')),
  batch_transition_kind NOT NULL CHECK(batch_transition_kind IN
    ('custody-terminal','generation-loss-terminal',
      'custody-recovery-retirement','fresh-origin')),
  receipt_batch_id UNIQUE, batch_completion_digest, transition_replay_digest,
  ordered_authority_receipt_set_digest, verified_scope_checkpoint_digest,
  applied_mutation_plan_digest,
  fresh_handoff_id UNIQUE, fresh_handoff_digest, fresh_handoff_key NOT NULL,
  fresh_project_session_id, fresh_run_id, fresh_agent_id, fresh_source_mode,
  fresh_apply_plan_digest, new_custody_id UNIQUE, new_custody_semantic_digest,
  new_custody_source_ref_digest, fresh_generation_loss_id,
  fresh_generation_loss_after_revision,
  fresh_generation_loss_after_semantic_digest,
  fresh_generation_loss_after_source_ref_digest,
  fresh_generation_loss_after_key NOT NULL, local_write_set_digest,
  apply_json, apply_digest UNIQUE, applied_at,
  UNIQUE(apply_id,apply_digest),
  UNIQUE(apply_id,apply_digest,apply_kind),
  UNIQUE(apply_id,receipt_batch_id),
  UNIQUE(apply_id,apply_digest,receipt_batch_id),
  UNIQUE(apply_id,fresh_handoff_id),
  UNIQUE(apply_id,apply_digest,fresh_handoff_id),
  UNIQUE(apply_id,apply_digest,fresh_handoff_id,apply_kind),
  UNIQUE(apply_id,apply_digest,new_custody_id,new_custody_semantic_digest,
    new_custody_source_ref_digest),
  UNIQUE(apply_id,apply_digest,fresh_generation_loss_after_key),
  UNIQUE(apply_id,apply_digest,fresh_project_session_id,fresh_run_id,
    fresh_agent_id,fresh_generation_loss_id,
    fresh_generation_loss_after_revision,
    fresh_generation_loss_after_semantic_digest,
    fresh_generation_loss_after_source_ref_digest),
  FOREIGN KEY(receipt_batch_id,apply_id,batch_transition_kind,apply_kind,
      transition_replay_digest,applied_mutation_plan_digest,fresh_handoff_key)
    REFERENCES lifecycle_receipt_batches(
      batch_id,planned_apply_id,transition_kind,planned_apply_kind,
      transition_replay_digest,mutation_plan_digest,fresh_handoff_key),
  FOREIGN KEY(receipt_batch_id,batch_completion_digest,
      ordered_authority_receipt_set_digest,verified_scope_checkpoint_digest)
    REFERENCES lifecycle_receipt_batch_authorizations(
      batch_id,batch_completion_digest,ordered_authority_receipt_set_digest,
      verified_scope_checkpoint_digest),
  FOREIGN KEY(fresh_handoff_id,fresh_handoff_digest,apply_id,
      fresh_project_session_id,fresh_run_id,fresh_agent_id,fresh_source_mode,
      new_custody_id,
      new_custody_semantic_digest,new_custody_source_ref_digest,
      fresh_apply_plan_digest,fresh_generation_loss_after_key)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
      agent_id,source_mode,new_custody_id,new_custody_semantic_digest,
      new_custody_source_ref_digest,fresh_apply_plan_digest,
      affected_generation_loss_after_key),
  FOREIGN KEY(fresh_handoff_id,apply_id,fresh_generation_loss_id,
      fresh_generation_loss_after_revision,
      fresh_generation_loss_after_semantic_digest,
      fresh_generation_loss_after_source_ref_digest)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,planned_apply_id,affected_generation_loss_id,
      affected_generation_loss_after_revision,
      affected_generation_loss_after_semantic_digest,
      affected_generation_loss_after_source_ref_digest),
  CHECK((apply_kind='terminal' AND
      batch_transition_kind IN ('custody-terminal','generation-loss-terminal',
        'custody-recovery-retirement') AND receipt_batch_id IS NOT NULL AND
      batch_completion_digest IS NOT NULL AND
      transition_replay_digest IS NOT NULL AND
      ordered_authority_receipt_set_digest IS NOT NULL AND
      verified_scope_checkpoint_digest IS NOT NULL AND
      applied_mutation_plan_digest IS NOT NULL AND fresh_handoff_id IS NULL AND
      fresh_handoff_digest IS NULL AND fresh_handoff_key='none' AND
      fresh_project_session_id IS NULL AND
      fresh_run_id IS NULL AND fresh_agent_id IS NULL AND
      fresh_source_mode IS NULL AND fresh_apply_plan_digest IS NULL AND
      new_custody_id IS NULL AND new_custody_semantic_digest IS NULL AND
      new_custody_source_ref_digest IS NULL AND
      fresh_generation_loss_id IS NULL AND
      fresh_generation_loss_after_revision IS NULL AND
      fresh_generation_loss_after_semantic_digest IS NULL AND
      fresh_generation_loss_after_source_ref_digest IS NULL AND
      fresh_generation_loss_after_key='none') OR
    (apply_kind='terminal-fresh' AND
      batch_transition_kind='custody-terminal' AND receipt_batch_id IS NOT NULL AND
      batch_completion_digest IS NOT NULL AND
      transition_replay_digest IS NOT NULL AND
      ordered_authority_receipt_set_digest IS NOT NULL AND
      verified_scope_checkpoint_digest IS NOT NULL AND
      applied_mutation_plan_digest IS NOT NULL AND fresh_handoff_id IS NOT NULL AND
      fresh_handoff_digest IS NOT NULL AND
      fresh_handoff_key=fresh_handoff_digest AND
      fresh_project_session_id IS NOT NULL AND
      fresh_run_id IS NOT NULL AND fresh_agent_id IS NOT NULL AND
      fresh_source_mode='terminalize-nonfinal-custody' AND
      fresh_apply_plan_digest IS NOT NULL AND
      new_custody_id IS NOT NULL AND new_custody_semantic_digest IS NOT NULL AND
      new_custody_source_ref_digest IS NOT NULL AND
      ((fresh_generation_loss_after_key='none' AND
          fresh_generation_loss_id IS NULL AND
          fresh_generation_loss_after_revision IS NULL AND
          fresh_generation_loss_after_semantic_digest IS NULL AND
          fresh_generation_loss_after_source_ref_digest IS NULL) OR
        (fresh_generation_loss_after_key<>'none' AND
          fresh_generation_loss_id IS NOT NULL AND
          fresh_generation_loss_after_revision IS NOT NULL AND
          fresh_generation_loss_after_semantic_digest IS NOT NULL AND
          fresh_generation_loss_after_source_ref_digest=
            fresh_generation_loss_after_key))) OR
    (apply_kind='fresh' AND batch_transition_kind='fresh-origin' AND
      receipt_batch_id IS NOT NULL AND
      batch_completion_digest IS NOT NULL AND
      transition_replay_digest IS NOT NULL AND
      ordered_authority_receipt_set_digest IS NOT NULL AND
      verified_scope_checkpoint_digest IS NOT NULL AND
      applied_mutation_plan_digest IS NOT NULL AND fresh_handoff_id IS NOT NULL AND
      fresh_handoff_digest IS NOT NULL AND
      fresh_handoff_key=fresh_handoff_digest AND
      fresh_project_session_id IS NOT NULL AND
      fresh_run_id IS NOT NULL AND fresh_agent_id IS NOT NULL AND
      fresh_source_mode IN ('reuse-final-custody','open-generation-loss') AND
      fresh_apply_plan_digest IS NOT NULL AND
      new_custody_id IS NOT NULL AND new_custody_semantic_digest IS NOT NULL AND
      new_custody_source_ref_digest IS NOT NULL AND
      applied_mutation_plan_digest=fresh_apply_plan_digest AND
      ((fresh_source_mode='reuse-final-custody' AND
          fresh_generation_loss_after_key='none' AND
          fresh_generation_loss_id IS NULL AND
          fresh_generation_loss_after_revision IS NULL AND
          fresh_generation_loss_after_semantic_digest IS NULL AND
          fresh_generation_loss_after_source_ref_digest IS NULL) OR
        (fresh_source_mode='open-generation-loss' AND
          fresh_generation_loss_after_key<>'none' AND
          fresh_generation_loss_id IS NOT NULL AND
          fresh_generation_loss_after_revision IS NOT NULL AND
          fresh_generation_loss_after_semantic_digest IS NOT NULL AND
          fresh_generation_loss_after_source_ref_digest=
            fresh_generation_loss_after_key))))
)

lifecycle_review_adoption_reservations(
  reservation_id PRIMARY KEY, reservation_digest UNIQUE,
  project_session_id, run_id, agent_id, custody_id,
  finalized_custody_revision, target_generation,
  predecessor_binding_generation, predecessor_binding_digest,
  terminal_sequence_high_water, lifecycle_adoption_evidence_digest,
  review_decision_json, review_decision_digest,
  certification_cut_json, certification_cut_digest, certification_cut_key,
  recovery_source_kind CHECK(
    recovery_source_kind IN ('none','custody','generation-loss')),
  recovery_from_custody_id, recovery_from_custody_revision,
  recovery_from_generation_loss_id, recovery_from_generation_loss_revision,
  recovery_source_ref_digest,
  decision_loss_after_id, decision_loss_after_revision,
  decision_loss_after_semantic_digest, decision_loss_after_source_ref_digest,
  decision_loss_after_key, decision_loss_effect_key NOT NULL,
  recovery_source_decision_json, recovery_source_decision_digest,
  local_write_set_digest, reservation_json, created_at,
  UNIQUE(reservation_id,reservation_digest),
  UNIQUE(reservation_id,reservation_digest,decision_loss_effect_key),
  UNIQUE(reservation_id,reservation_digest,decision_loss_effect_key,
    decision_loss_after_id,decision_loss_after_revision,
    decision_loss_after_semantic_digest,
    decision_loss_after_source_ref_digest),
  UNIQUE(reservation_digest,project_session_id,run_id,agent_id,custody_id,
    finalized_custody_revision,review_decision_digest,certification_cut_key,
    decision_loss_after_key),
  UNIQUE(reservation_digest,decision_loss_after_id,decision_loss_after_revision,
    decision_loss_after_semantic_digest,decision_loss_after_source_ref_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,
    finalized_custody_revision),
  CHECK(certification_cut_key IS NOT NULL AND
    decision_loss_after_key IS NOT NULL AND
    decision_loss_effect_key IS NOT NULL),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id)
    REFERENCES lifecycle_rotation_custodies(
      project_session_id,run_id,agent_id,custody_id),
  CHECK((certification_cut_digest IS NULL AND certification_cut_key='none') OR
    (certification_cut_digest IS NOT NULL AND
      certification_cut_key=certification_cut_digest)),
  CHECK((recovery_source_kind='none' AND
      recovery_from_custody_id IS NULL AND
      recovery_from_custody_revision IS NULL AND
      recovery_from_generation_loss_id IS NULL AND
      recovery_from_generation_loss_revision IS NULL AND
      recovery_source_ref_digest IS NULL AND
      decision_loss_after_id IS NULL AND decision_loss_after_revision IS NULL AND
      decision_loss_after_semantic_digest IS NULL AND
      decision_loss_after_source_ref_digest IS NULL AND
      decision_loss_after_key='none' AND
      decision_loss_effect_key='none' AND
      recovery_source_decision_json IS NULL AND
      recovery_source_decision_digest IS NULL) OR
    (recovery_source_kind='custody' AND
      recovery_from_custody_id IS NOT NULL AND
      recovery_from_custody_revision IS NOT NULL AND
      recovery_from_generation_loss_id IS NULL AND
      recovery_from_generation_loss_revision IS NULL AND
      recovery_source_ref_digest IS NOT NULL AND
      decision_loss_after_id IS NULL AND decision_loss_after_revision IS NULL AND
      decision_loss_after_semantic_digest IS NULL AND
      decision_loss_after_source_ref_digest IS NULL AND
      decision_loss_after_key='none' AND
      decision_loss_effect_key='none' AND
      recovery_source_decision_json IS NOT NULL AND
      recovery_source_decision_digest IS NOT NULL) OR
    (recovery_source_kind='generation-loss' AND
      recovery_from_custody_id IS NULL AND
      recovery_from_custody_revision IS NULL AND
      recovery_from_generation_loss_id IS NOT NULL AND
      recovery_from_generation_loss_revision IS NOT NULL AND
      recovery_source_ref_digest IS NOT NULL AND
      decision_loss_after_id=recovery_from_generation_loss_id AND
      decision_loss_after_revision IS NOT NULL AND
      decision_loss_after_semantic_digest IS NOT NULL AND
      decision_loss_after_source_ref_digest IS NOT NULL AND
      decision_loss_after_key=decision_loss_after_source_ref_digest AND
      decision_loss_effect_key<>'none' AND
      recovery_source_decision_json IS NOT NULL AND
      recovery_source_decision_digest IS NOT NULL)),
  FOREIGN KEY(project_session_id,run_id,agent_id,recovery_from_custody_id,
      recovery_from_custody_revision,recovery_source_ref_digest)
    REFERENCES lifecycle_rotation_custody_revisions(
      project_session_id,run_id,agent_id,custody_id,revision,
      source_ref_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,
      recovery_from_generation_loss_id,recovery_from_generation_loss_revision,
      recovery_source_ref_digest)
    REFERENCES lifecycle_generation_loss_revisions(
      project_session_id,run_id,agent_id,generation_loss_id,
      revision,source_ref_digest),
  CHECK((decision_loss_effect_key='none')=
    (decision_loss_after_id IS NULL))
)

~~~

## Receipt persistence and adoption


Lifecycle receipt persistence implements the lifecycle-receipt contract without a mutable row that can disagree with its history.
Custody and generation-loss identity rows are immutable; every state edge appends one semantic/journal revision and
CASes the exact head to that foreign-keyed tuple. Revision one has no predecessor, every successor names revision minus
one and its journal digest, and no terminal head accepts another successor. All shown tables are `STRICT`; the generated
DDL marks every field nonnull except the exact discriminator/null arms shown here and in the Agent Fabric contract. UPDATE/DELETE is
denied for identity, revision, scope-admission outbox/resolution, batch, effect, intent, authority-receipt, checkpoint,
authorization, reservation, handoff, commit and apply rows. Only head pointers use guarded UPDATE.

The TypeScript daemon validates closed RFC 8785 JCS, every the Agent Fabric contract domain- separated digest, authority attestation and
cross-object equality before opening the write transaction. SQLite enforces identities, exact composite foreign keys,
arm nullability, monotonic revisions, legal edge/cardinality and immutable rows. No trigger invokes a JavaScript hash
UDF: production keeps `PRAGMA trusted_schema=OFF`, so cryptographic validation cannot be delegated to an
unsafe/unavailable application-defined trigger function. Direct-SQL negative fixtures still prove every relational and
state invariant.

Before the first lifecycle identity or issue for a project/session/run, the daemon writes only one immutable
`lifecycle_scope_admission_outbox` row. Its worker point-reads or idempotently admits that exact scope at the external
authority, verifies the returned authenticated zero-receipt checkpoint and project-namespace membership, then atomically
inserts the local admitted scope, checkpoint/head and admission resolution. Return loss and every local insert boundary
replay from the retained outbox; changed scope bytes conflict. No custody, loss, issue, handoff, receipt batch or apply
may precede that verified resolution.

The prepare transaction locks the exact current head/source rows, verifies one closed proof, and writes an immutable
review reservation or fresh handoff first when applicable. It then writes one immutable batch, its exact primary/linked
effects and one or two immutable intents. A custody batch has exactly one primary custody effect and at most one linked
loss effect; a standalone direct-open loss batch has exactly one primary loss effect. Adopted true-chair custody has
ordinal-two review intent/reservation; terminal-fresh instead has ordinal-two fresh-origin, and pure fresh has
ordinal-one fresh-origin. No lifecycle, provider, review, archive, history, audit or issue-consumption mutation occurs
before authority. No external call occurs while SQLite is locked.

Each intent equality-binds its subject owner kind, identity and revision to the exact typed effect in the same batch.
The generated SQLite DDL declares `lifecycle_completion_effect_set_exact` as a `BEFORE INSERT` guard on completion:
custody has exactly one primary custody effect plus only its declared optional linked loss and terminal-fresh secondary
fresh-origin effect; standalone loss has exactly one primary loss effect; retirement has exactly one primary retirement
effect; pure fresh has exactly one primary fresh-origin effect; every other effect table is empty for that arm. A
missing, crossed or extra effect aborts with `lifecycle-effect-set-incomplete`. Every custody, generation-loss,
retirement and fresh-origin effect table also rejects insertion after completion with `lifecycle-effect-set-closed`.
Completion is therefore both membership proof and an anti-extra fence; the daemon independently validates the canonical
effect-set digest without a trigger hash UDF.

The apply marker remains the final statement. The generated SQLite DDL declares `lifecycle_apply_post_state_complete` as
its `BEFORE INSERT` guard and aborts with `lifecycle-apply-post-state-incomplete` unless the selected arm is complete.
Custody terminal requires its exact effect-selected final revision and current head, its declared linked-loss
revision/head when present, and its review binding when the batch selected review. Standalone loss requires its primary
final revision/head. Retirement requires its exact effect-selected retirement result. Terminal-fresh additionally
requires the exact new custody revision-one/head and fresh commit, plus its declared affected-loss revision/head when
present. Pure reuse-final fresh requires its exact new custody revision-one/head and fresh commit; pure open-loss fresh
also requires its exact recovery-in-progress loss revision/head. Child-to-apply foreign keys in custody/loss revisions,
review binding, fresh commit and retirement result are `DEFERRABLE INITIALLY DEFERRED`; the guard never creates a
missing child.

The worker point-reads before append, appends only on authoritative absence and point-reads again after a return, throw
or timeout. Exact verified results insert separate immutable `lifecycle_authority_receipts`; intent rows never mutate.
Once all declared receipts belong to one verified pinned scope checkpoint, one `lifecycle_receipt_batch_authorizations`
row is inserted. The apply transaction then equality-checks the current journal and complete semantic write/effect set,
appends final revision journal(s), advances exact heads, performs every reserved review/archive/fresh write and inserts
one `lifecycle_transition_applies` row. Derived state is prepared, authority-complete or applied from child-row
existence; no state column duplicates it. Exact pre-state or exact post-state replay succeeds; any third state fails
integrity. Provider no-effect/history/ audit and linked loss state are never changed before apply.

Hydration is read-only and starts at the authenticated project namespace, not local custody rows. It resolves every
historical scope checkpoint named by that pinned namespace, pages each immutable checkpoint through the 256-row API and
reconciles every zero-receipt member to its exact local immutable admission outbox/resolution/scope tuple before
reconciling the external set against local pending/applied intents. Whole- custody/run deletion, extra external rows,
missing committed receipts, chain/ head/count/set drift, crossed authority or invalid attestation is `SNAPSHOT_INVALID`.
A pending intent alone may be externally absent. Only after successful hydration may `LifecycleReceiptRecoveryService`
resume append or apply; point lookup is response-loss recovery, never completeness proof.

The review reservation is immutable and has no batch back-pointer or mutable consumed state; its exact batch points one
way to it and the apply proves consumption. A generation-loss reservation names the planned linked effect key and after
tuple without foreign-keying the not-yet-materialized revision. The same prepare batch equality-copies that tuple and
binds it, deferred, to its exact linked effect; the apply-time review binding equality-copies the batch tuple and is
deferred to the apply marker. It freezes decision/cut/high-water/predecessor at the adoption linearization point while
permitting later provider terminals as post-cut. Review cut, successor binding and rebind receipt equality-copy the
decision and ordinal-two external receipt; recovery never rereads later high-water or re- enters the review owner.

Fresh preparation and handoff are immutable. A nonfinal awaiting-boundary/ prepared custody with zero dispatch uses
`fresh-handoff-superseded`: the source and issue remain unchanged while its custody-terminal batch is pending, then one
`terminal-fresh` apply finalizes the source, creates new custody revision one, inserts the commit and derives issue
consumption. A finalized custody or open generation loss uses one externally authenticated `fresh-origin` batch and one
`fresh` apply from the same handoff; the loss moves to recovery-in-progress, not terminal. Issue state is derived as
active, commit- pending, consumed, revoked or expired; a handoff freezes later revoke/expiry. Composite keys enforce the
preparation/handoff/commit/issue/source/custody/ action bijection and both source arms without nullable-FK vacuity.

Verification mutates every proof arm and arm discriminator; faults each
prepare/append/reread/receipt/checkpoint/authorization/apply statement; injects success-then-throw, invalid attestation
and live-head advance during pinned paging; deletes whole custody/run histories; advances review high-water after
reservation; and crosses request, semantic/journal revision, source, linked loss, issue, preparation, handoff, commit,
decision, cut, effect, receipt and apply. Every changed-input replay fails for the right reason.

Context observation classification is closed: `generation-advance`, `context- advance`, `replay` or
`reordered-observation`. Adapter input is a positive provider generation plus nonnegative normalised context revision
and stable source event ID/evidence digest. Natural uniqueness makes replay return the one existing classification/audit
row and bounds audit growth. The high- water trigger accepts equal replay, requires strict revision increase for
same-generation context-advance, and makes a lower generation or lower same- generation revision append audit only with
no high-water/lifecycle change. A greater provider generation creates generation-advance regardless of context revision
and installs that generation's baseline. Final CAS repeats the order. Only provider/context high-water moves from this
telemetry. Principal and bridge high-water move solely from authenticated daemon custody reservation/adoption tuples;
provider integers cannot infer either authority generation.

`abandon_kind_code` is the nonnull sentinel `none` outside terminal abandoned. Direct `open -> abandoned` requires
`direct-open` and both recovery-action columns null. Abandon from recovery-in-progress requires `recovery-attempt` and a
complete global provider action pair equal to the active recovery custody. Recovered-adopted and every nonterminal state
require `none`. Composite CHECK/foreign keys reject half-null, crossed adapter/action and invented direct-open actions.
`lifecycle_generation_loss_revisions` has no free terminal-disposition column: public disposition is derived exactly
from state plus abandon kind. Custody `disposition_code` is `none` before finalized and exactly one closed terminal
value at finalized; journal/head triggers enforce the Agent Fabric contract edge table. Partial unique indexes prevent a second
nonfinal custody or nonterminal loss for one agent.

Adoption-delivery history is unique inside its custody. `active_owner=1` only while that custody is nonfinal;
terminalisation flips it to zero in the same transaction. The partial index permits only one nonfinal custody to own a
delivery/generation, while a later retry may reinsert the same predecessor under its new custody without deleting
immutable history.

No delivery schema/state is added. `successor-pending` is the pure joined projection for which delivery state is
`ready`, delivery recipient equals the agent, and the active lifecycle-delivery owner is exactly one of: a nonfinal
custody, a standalone open generation loss, or a recovery-in-progress loss whose `active_recovery_custody_id` exactly
names that nonfinal custody. A standalone recovery-in-progress loss or crossed/multiple unrelated rows is an integrity
failure and remains claim-fenced. The existing recipient/state/ sequence and lifecycle-owner indexes serve it.
Mailbox/operator reads expose that row as stored state ready plus routing disposition successor-pending; receipt ready
counts include it and no successor-pending counter exists. Claim reuses the same predicate under its CAS. Adoption
finalises custody and any linked loss without mutating the ready delivery, clearing the disposition and making it
claimable; abandon updates every matching ready row to abandoned with reason/watermark before finalising custody.
Enqueue before/after the delivery cut needs no extra field.

Identity, bridge and per-provider-generation context high-water rows plus custody identity/source/checkpoint/target
fields are immutable except through their named CAS transactions. Custody target provider/principal generations are each
the prior run/agent-global high-water plus one; target bridge is the prior run/agent/owner-kind bridge high-water plus
one. Each high-water increments in the reservation transaction. A superseded, quarantined or abandoned attempt does not
return a number. Activation equality- CASes the exact provider-session, capability/action, adapter/contract and bridge
row/revision snapshots plus, for a true chair, exact project-session/run/chair- lease generations. It then installs the
reserved targets. Source-plus-one, skipped, reused or crossed-owner values fail.

Self-request carries no turn ID. The transaction first commits delivery claim- expiry/reclaim and
membership/delivery-watermark housekeeping, then derives one active caller turn from the authenticated capability plus
current bridge/provider generations. Zero, multiple, foreign or quarantined matches reject, as does any second active or
quarantined predecessor. Terminal predecessor states are released and revoked. It then changes every active agent-owned
write lease to lifecycle-quarantined before computing the checkpoint/precondition digest; fences claims, records
delivery_cut_watermark, captures only claimed predecessor delivery IDs/generations in adoption_delivery_set_digest, and
captures the exact daemon-validated checkpoint, `open_work_set_digest` and all other revision/set digests. Open work
includes every nonterminal request-result obligation and its revision, including provider-accepted/unconsumed callbacks;
rechecks the post-housekeeping lease/freeze set; inserts custody; fences new claims/turns; sets suspended; and commits
accepted-suspended without adapter I/O. Exact replay returns that immutable receipt; current lifecycle is a read. Every
delivery-claim transaction performs the same expiry/reclaim/watermark housekeeping and rechecks lifecycle freeze
immediately before its claim CAS. The request transaction inserts the exact claimed rows into
lifecycle_custody_adoption_deliveries with contiguous ordinals/source state; their canonical digest must equal
adoption_delivery_set_digest. Adoption CASes those foreign-keyed IDs/generations, never a fresh query over current
claims.

Delivery enqueue remains durable, but ready/unclaimed rows at the cut and later enqueues are successor-pending and
excluded from checkpoint/precondition/ adoption digests. They cannot stale rotation; adoption makes the same rows
claimable without replay. Delivery claim/ack and write acquisition are trigger- denied while custody owns the agent. The
old grant may finish only the captured lifecycle call and bounded reads. The staged grant exposes only the existing
launch.attest descriptor bound to custody/action, challenge and checkpoint/open-work vector. Every other
agent/task/mailbox/authority/ write/turn/barrier mutation is denied.

Triggers implement exactly the Agent Fabric contract state-edge table and dispositions adopted, no-effect, quarantined, superseded and
abandoned. No state may skip an edge. awaiting-boundary waits for the captured caller and every predecessor turn to
reach a terminal status at its exact generation. An operator-created fresh rotation stores null caller turn and is not
inserted until every predecessor is terminal. Predispatch no-effect needs the durable zero-dispatch journal.
Postdispatch no-effect needs the activated adapter contract's authenticated closed proof; timeout or absence never
suffices.

Final no-effect/superseded transactions revoke the staged replacement, clear only this custody's freeze-owner rows,
retain the valid predecessor and set the agent ready. Quarantined finalisation retains its freeze-owner rows, keeps the
agent suspended and sets recovery-required. Abandon uses the archival owner below. Generic Resume cannot execute any of
these exits.

Rotation dispatch always creates a distinct provider context under the new action/custody. Same-history attach/resume is
accepted only by crash recovery for the same custody and cannot satisfy rotation. The adopted bridge receives only the
bounded canonical checkpoint/handoff after commit; no predecessor transcript or hidden provider history is copied.

Dispatch marks the one-time volatile handoff before I/O. The replacement session answers challenge and
checkpoint/task/mailbox/child/open-work vector through launch.attest. The daemon verifies and retains the exact
successor volatile bridge before beginning the final database CAS. Adoption rechecks custody, source/high-water targets
and every precondition, inserts provider lineage, swaps a child through agent_bridge_state or a chair through
launched_chair_bridge_state, activates the staged capability, revokes the old principal/capability, transfers the exact
open-work obligations unchanged and sets ready. Postcommit cleanup retires the exact old volatile bridge; a crash-left
transport has no credential authority. Existing write leases remain lifecycle-quarantined. A true-chair adoption
captures the review certification cut and performs same-subject binding rebind-or-stale in the same serialization point.
Review actions/ambiguity never block or roll back adoption; old actions retain their normal recovery owner.
