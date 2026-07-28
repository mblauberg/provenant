#!/usr/bin/env python3
"""Focused executable oracle for transition-core Leads 4, 5, and 6.

The schema transcribes only the copied arm, effect-membership, completion-fence,
and apply-last witnesses exercised below. The complete normative DDL remains in
the hardening specifications; this fixture is deliberately isolated from the Lead-3/v2 oracle.
"""

from __future__ import annotations

import sqlite3
import unittest


SCHEMA = r"""
CREATE TABLE lifecycle_fresh_recovery_handoffs(
  handoff_id TEXT PRIMARY KEY,
  planned_apply_id TEXT NOT NULL,
  source_mode TEXT NOT NULL CHECK(source_mode IN
    ('terminalize-nonfinal-custody','reuse-final-custody',
     'open-generation-loss')),
  fresh_apply_plan_digest TEXT NOT NULL,
  new_custody_id TEXT NOT NULL,
  affected_generation_loss_id TEXT,
  UNIQUE(handoff_id,planned_apply_id,source_mode,fresh_apply_plan_digest,
    new_custody_id,affected_generation_loss_id),
  CHECK((source_mode IN
      ('terminalize-nonfinal-custody','reuse-final-custody') AND
      affected_generation_loss_id IS NULL) OR
    (source_mode='open-generation-loss' AND
      affected_generation_loss_id IS NOT NULL))
);

CREATE TABLE lifecycle_receipt_batches(
  batch_id TEXT PRIMARY KEY,
  planned_apply_id TEXT NOT NULL UNIQUE,
  transition_kind TEXT NOT NULL CHECK(transition_kind IN
    ('custody-terminal','generation-loss-terminal',
     'custody-recovery-retirement','fresh-origin')),
  planned_apply_kind TEXT NOT NULL CHECK(planned_apply_kind IN
    ('terminal','terminal-fresh','fresh')),
  mutation_plan_digest TEXT NOT NULL,
  handoff_id TEXT,
  fresh_source_mode TEXT,
  fresh_apply_plan_digest TEXT,
  new_custody_id TEXT,
  affected_generation_loss_id TEXT,
  UNIQUE(batch_id,transition_kind,planned_apply_kind),
  UNIQUE(batch_id,planned_apply_id,transition_kind,planned_apply_kind,
    mutation_plan_digest),
  CHECK(
    (planned_apply_kind='terminal' AND transition_kind IN
      ('custody-terminal','generation-loss-terminal',
       'custody-recovery-retirement') AND handoff_id IS NULL AND
      fresh_source_mode IS NULL AND fresh_apply_plan_digest IS NULL AND
      new_custody_id IS NULL AND affected_generation_loss_id IS NULL) OR
    (planned_apply_kind='terminal-fresh' AND
      transition_kind='custody-terminal' AND handoff_id IS NOT NULL AND
      fresh_source_mode='terminalize-nonfinal-custody' AND
      fresh_apply_plan_digest IS NOT NULL AND new_custody_id IS NOT NULL AND
      affected_generation_loss_id IS NULL) OR
    (planned_apply_kind='fresh' AND transition_kind='fresh-origin' AND
      handoff_id IS NOT NULL AND fresh_source_mode IN
        ('reuse-final-custody','open-generation-loss') AND
      fresh_apply_plan_digest IS NOT NULL AND new_custody_id IS NOT NULL AND
      ((fresh_source_mode='reuse-final-custody' AND
          affected_generation_loss_id IS NULL) OR
       (fresh_source_mode='open-generation-loss' AND
          affected_generation_loss_id IS NOT NULL)))),
  FOREIGN KEY(handoff_id,planned_apply_id,fresh_source_mode,
      fresh_apply_plan_digest,new_custody_id,affected_generation_loss_id)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,planned_apply_id,source_mode,fresh_apply_plan_digest,
      new_custody_id,affected_generation_loss_id)
);

CREATE TABLE lifecycle_receipt_custody_effects(
  batch_id TEXT NOT NULL,
  effect_digest TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  final_revision INTEGER NOT NULL,
  PRIMARY KEY(batch_id,effect_digest),
  UNIQUE(batch_id,effect_digest,custody_id,final_revision),
  FOREIGN KEY(batch_id) REFERENCES lifecycle_receipt_batches(batch_id)
);

CREATE TABLE lifecycle_receipt_generation_loss_effects(
  batch_id TEXT NOT NULL,
  role TEXT NOT NULL CHECK(role IN ('primary','linked')),
  effect_digest TEXT NOT NULL,
  generation_loss_id TEXT NOT NULL,
  final_revision INTEGER NOT NULL,
  PRIMARY KEY(batch_id,role,effect_digest),
  UNIQUE(batch_id,role,effect_digest,generation_loss_id,final_revision),
  FOREIGN KEY(batch_id) REFERENCES lifecycle_receipt_batches(batch_id)
);

CREATE TABLE lifecycle_receipt_recovery_retirement_effects(
  batch_id TEXT NOT NULL,
  effect_digest TEXT NOT NULL,
  retirement_id TEXT NOT NULL,
  retirement_revision INTEGER NOT NULL CHECK(retirement_revision=1),
  PRIMARY KEY(batch_id,effect_digest),
  UNIQUE(batch_id,effect_digest,retirement_id,retirement_revision),
  FOREIGN KEY(batch_id) REFERENCES lifecycle_receipt_batches(batch_id)
);

CREATE TABLE lifecycle_receipt_fresh_origin_effects(
  batch_id TEXT NOT NULL,
  effect_digest TEXT NOT NULL,
  new_custody_id TEXT NOT NULL,
  new_custody_revision INTEGER NOT NULL CHECK(new_custody_revision=1),
  affected_generation_loss_id TEXT,
  PRIMARY KEY(batch_id,effect_digest),
  UNIQUE(batch_id,effect_digest,new_custody_id,new_custody_revision),
  UNIQUE(batch_id,effect_digest,new_custody_id,new_custody_revision,
    affected_generation_loss_id),
  FOREIGN KEY(batch_id) REFERENCES lifecycle_receipt_batches(batch_id)
);

CREATE TABLE lifecycle_receipt_intents(
  batch_id TEXT NOT NULL,
  ordinal INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK(kind IN
    ('custody-terminal','generation-loss-terminal',
     'custody-recovery-retirement','review-adoption-decision','fresh-origin')),
  subject_owner_kind TEXT NOT NULL CHECK(subject_owner_kind IN
    ('custody','generation-loss','recovery-retirement')),
  subject_owner_id TEXT NOT NULL,
  subject_owner_revision INTEGER NOT NULL,
  custody_effect_digest TEXT,
  generation_loss_effect_role TEXT,
  generation_loss_effect_digest TEXT,
  recovery_retirement_effect_digest TEXT,
  fresh_origin_effect_digest TEXT,
  PRIMARY KEY(batch_id,ordinal),
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
      custody_effect_digest IS NULL AND
      generation_loss_effect_role IS NULL AND
      generation_loss_effect_digest IS NULL AND
      recovery_retirement_effect_digest IS NULL AND
      fresh_origin_effect_digest IS NOT NULL)),
  FOREIGN KEY(batch_id,custody_effect_digest,subject_owner_id,
      subject_owner_revision)
    REFERENCES lifecycle_receipt_custody_effects(
      batch_id,effect_digest,custody_id,final_revision),
  FOREIGN KEY(batch_id,generation_loss_effect_role,
      generation_loss_effect_digest,subject_owner_id,subject_owner_revision)
    REFERENCES lifecycle_receipt_generation_loss_effects(
      batch_id,role,effect_digest,generation_loss_id,final_revision),
  FOREIGN KEY(batch_id,recovery_retirement_effect_digest,subject_owner_id,
      subject_owner_revision)
    REFERENCES lifecycle_receipt_recovery_retirement_effects(
      batch_id,effect_digest,retirement_id,retirement_revision),
  FOREIGN KEY(batch_id,fresh_origin_effect_digest,subject_owner_id,
      subject_owner_revision)
    REFERENCES lifecycle_receipt_fresh_origin_effects(
      batch_id,effect_digest,new_custody_id,new_custody_revision)
);

CREATE TABLE lifecycle_receipt_batch_completions(
  batch_id TEXT PRIMARY KEY,
  transition_kind TEXT NOT NULL,
  planned_apply_kind TEXT NOT NULL,
  primary_custody_effect_digest TEXT,
  primary_loss_effect_role TEXT,
  primary_loss_effect_digest TEXT,
  primary_retirement_effect_digest TEXT,
  primary_fresh_origin_effect_digest TEXT,
  FOREIGN KEY(batch_id,transition_kind,planned_apply_kind)
    REFERENCES lifecycle_receipt_batches(
      batch_id,transition_kind,planned_apply_kind),
  FOREIGN KEY(batch_id,primary_custody_effect_digest)
    REFERENCES lifecycle_receipt_custody_effects(batch_id,effect_digest),
  FOREIGN KEY(batch_id,primary_loss_effect_role,primary_loss_effect_digest)
    REFERENCES lifecycle_receipt_generation_loss_effects(
      batch_id,role,effect_digest),
  FOREIGN KEY(batch_id,primary_retirement_effect_digest)
    REFERENCES lifecycle_receipt_recovery_retirement_effects(
      batch_id,effect_digest),
  FOREIGN KEY(batch_id,primary_fresh_origin_effect_digest)
    REFERENCES lifecycle_receipt_fresh_origin_effects(batch_id,effect_digest)
);

CREATE TRIGGER lifecycle_completion_effect_set_exact
BEFORE INSERT ON lifecycle_receipt_batch_completions
BEGIN
  SELECT RAISE(ABORT,'lifecycle-effect-set-incomplete')
  WHERE NOT (
    (NEW.transition_kind='custody-terminal' AND
      NEW.planned_apply_kind='terminal' AND
      NEW.primary_custody_effect_digest IS NOT NULL AND
      NEW.primary_loss_effect_role IS NULL AND
      NEW.primary_loss_effect_digest IS NULL AND
      NEW.primary_retirement_effect_digest IS NULL AND
      NEW.primary_fresh_origin_effect_digest IS NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects
        WHERE batch_id=NEW.batch_id)=1 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects
        WHERE batch_id=NEW.batch_id)=0) OR
    (NEW.transition_kind='custody-terminal' AND
      NEW.planned_apply_kind='terminal-fresh' AND
      NEW.primary_custody_effect_digest IS NOT NULL AND
      NEW.primary_loss_effect_role IS NULL AND
      NEW.primary_loss_effect_digest IS NULL AND
      NEW.primary_retirement_effect_digest IS NULL AND
      NEW.primary_fresh_origin_effect_digest IS NOT NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects
        WHERE batch_id=NEW.batch_id)=1 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects
        WHERE batch_id=NEW.batch_id)=1) OR
    (NEW.transition_kind='generation-loss-terminal' AND
      NEW.planned_apply_kind='terminal' AND
      NEW.primary_custody_effect_digest IS NULL AND
      NEW.primary_loss_effect_role='primary' AND
      NEW.primary_loss_effect_digest IS NOT NULL AND
      NEW.primary_retirement_effect_digest IS NULL AND
      NEW.primary_fresh_origin_effect_digest IS NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects
        WHERE batch_id=NEW.batch_id)=1 AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects
        WHERE batch_id=NEW.batch_id)=0) OR
    (NEW.transition_kind='custody-recovery-retirement' AND
      NEW.planned_apply_kind='terminal' AND
      NEW.primary_custody_effect_digest IS NULL AND
      NEW.primary_loss_effect_role IS NULL AND
      NEW.primary_loss_effect_digest IS NULL AND
      NEW.primary_retirement_effect_digest IS NOT NULL AND
      NEW.primary_fresh_origin_effect_digest IS NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects
        WHERE batch_id=NEW.batch_id)=1 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects
        WHERE batch_id=NEW.batch_id)=0) OR
    (NEW.transition_kind='fresh-origin' AND
      NEW.planned_apply_kind='fresh' AND
      NEW.primary_custody_effect_digest IS NULL AND
      NEW.primary_loss_effect_role IS NULL AND
      NEW.primary_loss_effect_digest IS NULL AND
      NEW.primary_retirement_effect_digest IS NULL AND
      NEW.primary_fresh_origin_effect_digest IS NOT NULL AND
      (SELECT count(*) FROM lifecycle_receipt_custody_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_generation_loss_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_recovery_retirement_effects
        WHERE batch_id=NEW.batch_id)=0 AND
      (SELECT count(*) FROM lifecycle_receipt_fresh_origin_effects
        WHERE batch_id=NEW.batch_id)=1)
  );
END;

CREATE TRIGGER lifecycle_custody_effect_set_closed
BEFORE INSERT ON lifecycle_receipt_custody_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-effect-set-closed')
  WHERE EXISTS (SELECT 1 FROM lifecycle_receipt_batch_completions
    WHERE batch_id=NEW.batch_id);
END;

CREATE TRIGGER lifecycle_loss_effect_set_closed
BEFORE INSERT ON lifecycle_receipt_generation_loss_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-effect-set-closed')
  WHERE EXISTS (SELECT 1 FROM lifecycle_receipt_batch_completions
    WHERE batch_id=NEW.batch_id);
END;

CREATE TRIGGER lifecycle_retirement_effect_set_closed
BEFORE INSERT ON lifecycle_receipt_recovery_retirement_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-effect-set-closed')
  WHERE EXISTS (SELECT 1 FROM lifecycle_receipt_batch_completions
    WHERE batch_id=NEW.batch_id);
END;

CREATE TRIGGER lifecycle_fresh_effect_set_closed
BEFORE INSERT ON lifecycle_receipt_fresh_origin_effects
BEGIN
  SELECT RAISE(ABORT,'lifecycle-effect-set-closed')
  WHERE EXISTS (SELECT 1 FROM lifecycle_receipt_batch_completions
    WHERE batch_id=NEW.batch_id);
END;

CREATE TABLE lifecycle_receipt_batch_authorizations(
  batch_id TEXT PRIMARY KEY,
  FOREIGN KEY(batch_id)
    REFERENCES lifecycle_receipt_batch_completions(batch_id)
);

CREATE TABLE lifecycle_transition_applies(
  apply_id TEXT PRIMARY KEY,
  apply_digest TEXT NOT NULL,
  receipt_batch_id TEXT NOT NULL UNIQUE,
  batch_transition_kind TEXT NOT NULL,
  apply_kind TEXT NOT NULL CHECK(apply_kind IN
    ('terminal','terminal-fresh','fresh')),
  applied_mutation_plan_digest TEXT NOT NULL,
  fresh_handoff_id TEXT,
  fresh_source_mode TEXT,
  fresh_apply_plan_digest TEXT,
  new_custody_id TEXT,
  fresh_generation_loss_id TEXT,
  UNIQUE(apply_id,apply_digest),
  CHECK(
    (apply_kind='terminal' AND batch_transition_kind IN
      ('custody-terminal','generation-loss-terminal',
       'custody-recovery-retirement') AND fresh_handoff_id IS NULL AND
      fresh_source_mode IS NULL AND fresh_apply_plan_digest IS NULL AND
      new_custody_id IS NULL AND fresh_generation_loss_id IS NULL) OR
    (apply_kind='terminal-fresh' AND
      batch_transition_kind='custody-terminal' AND
      fresh_handoff_id IS NOT NULL AND
      fresh_source_mode='terminalize-nonfinal-custody' AND
      fresh_apply_plan_digest IS NOT NULL AND new_custody_id IS NOT NULL AND
      fresh_generation_loss_id IS NULL) OR
    (apply_kind='fresh' AND batch_transition_kind='fresh-origin' AND
      fresh_handoff_id IS NOT NULL AND fresh_source_mode IN
        ('reuse-final-custody','open-generation-loss') AND
      fresh_apply_plan_digest IS NOT NULL AND new_custody_id IS NOT NULL AND
      applied_mutation_plan_digest=fresh_apply_plan_digest AND
      ((fresh_source_mode='reuse-final-custody' AND
          fresh_generation_loss_id IS NULL) OR
       (fresh_source_mode='open-generation-loss' AND
          fresh_generation_loss_id IS NOT NULL)))),
  FOREIGN KEY(receipt_batch_id,apply_id,batch_transition_kind,apply_kind,
      applied_mutation_plan_digest)
    REFERENCES lifecycle_receipt_batches(
      batch_id,planned_apply_id,transition_kind,planned_apply_kind,
      mutation_plan_digest),
  FOREIGN KEY(fresh_handoff_id,apply_id,fresh_source_mode,
      fresh_apply_plan_digest,new_custody_id,fresh_generation_loss_id)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,planned_apply_id,source_mode,fresh_apply_plan_digest,
      new_custody_id,affected_generation_loss_id),
  FOREIGN KEY(receipt_batch_id)
    REFERENCES lifecycle_receipt_batch_authorizations(batch_id)
);

CREATE TABLE lifecycle_rotation_custody_revisions(
  custody_id TEXT NOT NULL,
  revision INTEGER NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  receipt_batch_id TEXT,
  receipt_apply_id TEXT,
  receipt_apply_digest TEXT,
  origin_apply_id TEXT,
  origin_apply_digest TEXT,
  PRIMARY KEY(custody_id,revision),
  UNIQUE(custody_id,revision,semantic_digest,source_ref_digest,journal_digest),
  CHECK(
    (receipt_batch_id IS NOT NULL AND receipt_apply_id IS NOT NULL AND
      receipt_apply_digest IS NOT NULL AND origin_apply_id IS NULL AND
      origin_apply_digest IS NULL) OR
    (receipt_batch_id IS NULL AND receipt_apply_id IS NULL AND
      receipt_apply_digest IS NULL AND origin_apply_id IS NOT NULL AND
      origin_apply_digest IS NOT NULL)),
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest)
    REFERENCES lifecycle_transition_applies(apply_id,apply_digest)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(origin_apply_id,origin_apply_digest)
    REFERENCES lifecycle_transition_applies(apply_id,apply_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE lifecycle_rotation_custody_heads(
  custody_id TEXT PRIMARY KEY,
  current_revision INTEGER NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  FOREIGN KEY(custody_id,current_revision,semantic_digest,source_ref_digest,
      journal_digest)
    REFERENCES lifecycle_rotation_custody_revisions(
      custody_id,revision,semantic_digest,source_ref_digest,journal_digest)
);

CREATE TABLE lifecycle_generation_loss_revisions(
  generation_loss_id TEXT NOT NULL,
  revision INTEGER NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  receipt_batch_id TEXT,
  receipt_apply_id TEXT,
  receipt_apply_digest TEXT,
  origin_apply_id TEXT,
  origin_apply_digest TEXT,
  PRIMARY KEY(generation_loss_id,revision),
  UNIQUE(generation_loss_id,revision,semantic_digest,source_ref_digest,
    journal_digest),
  CHECK(
    (receipt_batch_id IS NOT NULL AND receipt_apply_id IS NOT NULL AND
      receipt_apply_digest IS NOT NULL AND origin_apply_id IS NULL AND
      origin_apply_digest IS NULL) OR
    (receipt_batch_id IS NULL AND receipt_apply_id IS NULL AND
      receipt_apply_digest IS NULL AND origin_apply_id IS NOT NULL AND
      origin_apply_digest IS NOT NULL)),
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest)
    REFERENCES lifecycle_transition_applies(apply_id,apply_digest)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(origin_apply_id,origin_apply_digest)
    REFERENCES lifecycle_transition_applies(apply_id,apply_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE lifecycle_generation_loss_heads(
  generation_loss_id TEXT PRIMARY KEY,
  current_revision INTEGER NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  FOREIGN KEY(generation_loss_id,current_revision,semantic_digest,
      source_ref_digest,journal_digest)
    REFERENCES lifecycle_generation_loss_revisions(
      generation_loss_id,revision,semantic_digest,source_ref_digest,
      journal_digest)
);

CREATE TABLE agent_lifecycle_recovery_retirements(
  retirement_id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL,
  effect_digest TEXT NOT NULL,
  apply_id TEXT NOT NULL,
  apply_digest TEXT NOT NULL,
  FOREIGN KEY(apply_id,apply_digest)
    REFERENCES lifecycle_transition_applies(apply_id,apply_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE lifecycle_fresh_rotation_commits(
  commit_id TEXT PRIMARY KEY,
  handoff_id TEXT NOT NULL,
  apply_id TEXT NOT NULL,
  apply_digest TEXT NOT NULL,
  new_custody_id TEXT NOT NULL,
  generation_loss_after_id TEXT,
  FOREIGN KEY(apply_id,apply_digest)
    REFERENCES lifecycle_transition_applies(apply_id,apply_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TRIGGER lifecycle_apply_post_state_complete
BEFORE INSERT ON lifecycle_transition_applies
BEGIN
  SELECT RAISE(ABORT,'lifecycle-apply-post-state-incomplete')
  WHERE NOT (
    (NEW.apply_kind='terminal' AND
      NEW.batch_transition_kind='custody-terminal' AND EXISTS (
        SELECT 1
        FROM lifecycle_receipt_custody_effects e
        JOIN lifecycle_rotation_custody_revisions r
          ON r.custody_id=e.custody_id AND r.revision=e.final_revision
         AND r.receipt_batch_id=e.batch_id
         AND r.receipt_apply_id=NEW.apply_id
         AND r.receipt_apply_digest=NEW.apply_digest
        JOIN lifecycle_rotation_custody_heads h
          ON h.custody_id=r.custody_id AND h.current_revision=r.revision
         AND h.semantic_digest=r.semantic_digest
         AND h.source_ref_digest=r.source_ref_digest
         AND h.journal_digest=r.journal_digest
        WHERE e.batch_id=NEW.receipt_batch_id
      )) OR
    (NEW.apply_kind='terminal' AND
      NEW.batch_transition_kind='generation-loss-terminal' AND EXISTS (
        SELECT 1
        FROM lifecycle_receipt_generation_loss_effects e
        JOIN lifecycle_generation_loss_revisions r
          ON r.generation_loss_id=e.generation_loss_id
         AND r.revision=e.final_revision AND r.receipt_batch_id=e.batch_id
         AND r.receipt_apply_id=NEW.apply_id
         AND r.receipt_apply_digest=NEW.apply_digest
        JOIN lifecycle_generation_loss_heads h
          ON h.generation_loss_id=r.generation_loss_id
         AND h.current_revision=r.revision
         AND h.semantic_digest=r.semantic_digest
         AND h.source_ref_digest=r.source_ref_digest
         AND h.journal_digest=r.journal_digest
        WHERE e.batch_id=NEW.receipt_batch_id AND e.role='primary'
      )) OR
    (NEW.apply_kind='terminal' AND
      NEW.batch_transition_kind='custody-recovery-retirement' AND EXISTS (
        SELECT 1
        FROM lifecycle_receipt_recovery_retirement_effects e
        JOIN agent_lifecycle_recovery_retirements r
          ON r.retirement_id=e.retirement_id AND r.batch_id=e.batch_id
         AND r.effect_digest=e.effect_digest AND r.apply_id=NEW.apply_id
         AND r.apply_digest=NEW.apply_digest
        WHERE e.batch_id=NEW.receipt_batch_id
      )) OR
    (NEW.apply_kind='terminal-fresh' AND EXISTS (
        SELECT 1
        FROM lifecycle_receipt_custody_effects e
        JOIN lifecycle_rotation_custody_revisions r
          ON r.custody_id=e.custody_id AND r.revision=e.final_revision
         AND r.receipt_batch_id=e.batch_id
         AND r.receipt_apply_id=NEW.apply_id
         AND r.receipt_apply_digest=NEW.apply_digest
        JOIN lifecycle_rotation_custody_heads h
          ON h.custody_id=r.custody_id AND h.current_revision=r.revision
         AND h.semantic_digest=r.semantic_digest
         AND h.source_ref_digest=r.source_ref_digest
         AND h.journal_digest=r.journal_digest
        WHERE e.batch_id=NEW.receipt_batch_id
      ) AND EXISTS (
        SELECT 1
        FROM lifecycle_receipt_fresh_origin_effects e
        JOIN lifecycle_rotation_custody_revisions r
          ON r.custody_id=e.new_custody_id
         AND r.revision=e.new_custody_revision
         AND r.origin_apply_id=NEW.apply_id
         AND r.origin_apply_digest=NEW.apply_digest
        JOIN lifecycle_rotation_custody_heads h
          ON h.custody_id=r.custody_id AND h.current_revision=r.revision
         AND h.semantic_digest=r.semantic_digest
         AND h.source_ref_digest=r.source_ref_digest
         AND h.journal_digest=r.journal_digest
        WHERE e.batch_id=NEW.receipt_batch_id
          AND e.new_custody_id=NEW.new_custody_id
      ) AND EXISTS (
        SELECT 1 FROM lifecycle_fresh_rotation_commits c
        WHERE c.handoff_id=NEW.fresh_handoff_id
          AND c.apply_id=NEW.apply_id AND c.apply_digest=NEW.apply_digest
          AND c.new_custody_id=NEW.new_custody_id
          AND c.generation_loss_after_id IS NULL
      )) OR
    (NEW.apply_kind='fresh' AND
      NEW.fresh_source_mode='reuse-final-custody' AND EXISTS (
        SELECT 1
        FROM lifecycle_receipt_fresh_origin_effects e
        JOIN lifecycle_rotation_custody_revisions r
          ON r.custody_id=e.new_custody_id
         AND r.revision=e.new_custody_revision
         AND r.origin_apply_id=NEW.apply_id
         AND r.origin_apply_digest=NEW.apply_digest
        JOIN lifecycle_rotation_custody_heads h
          ON h.custody_id=r.custody_id AND h.current_revision=r.revision
         AND h.semantic_digest=r.semantic_digest
         AND h.source_ref_digest=r.source_ref_digest
         AND h.journal_digest=r.journal_digest
        WHERE e.batch_id=NEW.receipt_batch_id
          AND e.new_custody_id=NEW.new_custody_id
          AND e.affected_generation_loss_id IS NULL
      ) AND EXISTS (
        SELECT 1 FROM lifecycle_fresh_rotation_commits c
        WHERE c.handoff_id=NEW.fresh_handoff_id
          AND c.apply_id=NEW.apply_id AND c.apply_digest=NEW.apply_digest
          AND c.new_custody_id=NEW.new_custody_id
          AND c.generation_loss_after_id IS NULL
      )) OR
    (NEW.apply_kind='fresh' AND
      NEW.fresh_source_mode='open-generation-loss' AND EXISTS (
        SELECT 1
        FROM lifecycle_receipt_fresh_origin_effects e
        JOIN lifecycle_rotation_custody_revisions r
          ON r.custody_id=e.new_custody_id
         AND r.revision=e.new_custody_revision
         AND r.origin_apply_id=NEW.apply_id
         AND r.origin_apply_digest=NEW.apply_digest
        JOIN lifecycle_rotation_custody_heads h
          ON h.custody_id=r.custody_id AND h.current_revision=r.revision
         AND h.semantic_digest=r.semantic_digest
         AND h.source_ref_digest=r.source_ref_digest
         AND h.journal_digest=r.journal_digest
        WHERE e.batch_id=NEW.receipt_batch_id
          AND e.new_custody_id=NEW.new_custody_id
          AND e.affected_generation_loss_id=NEW.fresh_generation_loss_id
      ) AND EXISTS (
        SELECT 1
        FROM lifecycle_receipt_fresh_origin_effects e
        JOIN lifecycle_generation_loss_revisions r
          ON r.generation_loss_id=e.affected_generation_loss_id
         AND r.origin_apply_id=NEW.apply_id
         AND r.origin_apply_digest=NEW.apply_digest
        JOIN lifecycle_generation_loss_heads h
          ON h.generation_loss_id=r.generation_loss_id
         AND h.current_revision=r.revision
         AND h.semantic_digest=r.semantic_digest
         AND h.source_ref_digest=r.source_ref_digest
         AND h.journal_digest=r.journal_digest
        WHERE e.batch_id=NEW.receipt_batch_id
          AND e.affected_generation_loss_id=NEW.fresh_generation_loss_id
      ) AND EXISTS (
        SELECT 1 FROM lifecycle_fresh_rotation_commits c
        WHERE c.handoff_id=NEW.fresh_handoff_id
          AND c.apply_id=NEW.apply_id AND c.apply_digest=NEW.apply_digest
          AND c.new_custody_id=NEW.new_custody_id
          AND c.generation_loss_after_id=NEW.fresh_generation_loss_id
      ))
  );
END;
"""


ARMS = (
    "custody",
    "loss",
    "retirement",
    "terminal-fresh",
    "fresh-reuse",
    "fresh-loss",
)


def database() -> sqlite3.Connection:
    db = sqlite3.connect(":memory:", isolation_level=None)
    db.execute("PRAGMA foreign_keys=ON")
    db.executescript(SCHEMA)
    return db


def arm_values(arm: str) -> dict[str, str | None]:
    transition = {
        "custody": "custody-terminal",
        "loss": "generation-loss-terminal",
        "retirement": "custody-recovery-retirement",
        "terminal-fresh": "custody-terminal",
        "fresh-reuse": "fresh-origin",
        "fresh-loss": "fresh-origin",
    }[arm]
    apply_kind = {
        "custody": "terminal",
        "loss": "terminal",
        "retirement": "terminal",
        "terminal-fresh": "terminal-fresh",
        "fresh-reuse": "fresh",
        "fresh-loss": "fresh",
    }[arm]
    source_mode = {
        "terminal-fresh": "terminalize-nonfinal-custody",
        "fresh-reuse": "reuse-final-custody",
        "fresh-loss": "open-generation-loss",
    }.get(arm)
    handoff_id = f"handoff-{arm}" if source_mode else None
    fresh_plan = f"fresh-plan-{arm}" if source_mode else None
    new_custody = f"new-custody-{arm}" if source_mode else None
    affected_loss = f"fresh-loss-{arm}" if arm == "fresh-loss" else None
    mutation = (
        fresh_plan if apply_kind == "fresh" else f"terminal-plan-{arm}"
    )
    return {
        "transition": transition,
        "apply_kind": apply_kind,
        "source_mode": source_mode,
        "handoff_id": handoff_id,
        "fresh_plan": fresh_plan,
        "new_custody": new_custody,
        "affected_loss": affected_loss,
        "mutation": mutation,
    }


def prepare_arm(db: sqlite3.Connection, arm: str) -> None:
    values = arm_values(arm)
    batch_id = f"batch-{arm}"
    apply_id = f"apply-{arm}"
    if values["handoff_id"]:
        db.execute(
            "INSERT INTO lifecycle_fresh_recovery_handoffs VALUES (?,?,?,?,?,?)",
            (
                values["handoff_id"], apply_id, values["source_mode"],
                values["fresh_plan"], values["new_custody"],
                values["affected_loss"],
            ),
        )
    db.execute(
        "INSERT INTO lifecycle_receipt_batches VALUES (?,?,?,?,?,?,?,?,?,?)",
        (
            batch_id, apply_id, values["transition"], values["apply_kind"],
            values["mutation"], values["handoff_id"], values["source_mode"],
            values["fresh_plan"], values["new_custody"],
            values["affected_loss"],
        ),
    )

    custody_digest = loss_role = loss_digest = retirement_digest = None
    fresh_digest = None
    if arm in ("custody", "terminal-fresh"):
        custody_digest = f"custody-effect-{arm}"
        db.execute(
            "INSERT INTO lifecycle_receipt_custody_effects VALUES (?,?,?,?)",
            (batch_id, custody_digest, f"old-custody-{arm}", 2),
        )
    elif arm == "loss":
        loss_role, loss_digest = "primary", "loss-effect-loss"
        db.execute(
            "INSERT INTO lifecycle_receipt_generation_loss_effects "
            "VALUES (?,?,?,?,?)",
            (batch_id, loss_role, loss_digest, "terminal-loss-loss", 2),
        )
    elif arm == "retirement":
        retirement_digest = "retirement-effect-retirement"
        db.execute(
            "INSERT INTO lifecycle_receipt_recovery_retirement_effects "
            "VALUES (?,?,?,1)",
            (batch_id, retirement_digest, "retirement-retirement"),
        )

    if values["handoff_id"]:
        fresh_digest = f"fresh-effect-{arm}"
        db.execute(
            "INSERT INTO lifecycle_receipt_fresh_origin_effects "
            "VALUES (?,?,?,1,?)",
            (batch_id, fresh_digest, values["new_custody"],
             values["affected_loss"]),
        )

    db.execute(
        "INSERT INTO lifecycle_receipt_batch_completions "
        "VALUES (?,?,?,?,?,?,?,?)",
        (
            batch_id, values["transition"], values["apply_kind"],
            custody_digest, loss_role, loss_digest, retirement_digest,
            fresh_digest,
        ),
    )
    db.execute(
        "INSERT INTO lifecycle_receipt_batch_authorizations VALUES (?)",
        (batch_id,),
    )


def insert_custody_state(
    db: sqlite3.Connection,
    *,
    custody_id: str,
    revision: int,
    batch_id: str | None,
    apply_id: str,
    apply_digest: str,
    head: bool,
) -> None:
    semantic = f"semantic-{custody_id}-{revision}"
    source = f"source-{custody_id}-{revision}"
    journal = f"journal-{custody_id}-{revision}"
    receipt = (batch_id, apply_id, apply_digest, None, None)
    if batch_id is None:
        receipt = (None, None, None, apply_id, apply_digest)
    db.execute(
        "INSERT INTO lifecycle_rotation_custody_revisions "
        "VALUES (?,?,?,?,?,?,?,?,?,?)",
        (custody_id, revision, semantic, source, journal, *receipt),
    )
    if head:
        db.execute(
            "INSERT INTO lifecycle_rotation_custody_heads VALUES (?,?,?,?,?)",
            (custody_id, revision, semantic, source, journal),
        )


def insert_loss_state(
    db: sqlite3.Connection,
    *,
    loss_id: str,
    revision: int,
    batch_id: str | None,
    apply_id: str,
    apply_digest: str,
    head: bool,
) -> None:
    semantic = f"semantic-{loss_id}-{revision}"
    source = f"source-{loss_id}-{revision}"
    journal = f"journal-{loss_id}-{revision}"
    receipt = (batch_id, apply_id, apply_digest, None, None)
    if batch_id is None:
        receipt = (None, None, None, apply_id, apply_digest)
    db.execute(
        "INSERT INTO lifecycle_generation_loss_revisions "
        "VALUES (?,?,?,?,?,?,?,?,?,?)",
        (loss_id, revision, semantic, source, journal, *receipt),
    )
    if head:
        db.execute(
            "INSERT INTO lifecycle_generation_loss_heads VALUES (?,?,?,?,?)",
            (loss_id, revision, semantic, source, journal),
        )


def materialize_arm(
    db: sqlite3.Connection,
    arm: str,
    *,
    custody_head: bool = True,
    fresh_head: bool = True,
    loss_head: bool = True,
    retirement_result: bool = True,
    fresh_commit: bool = True,
) -> None:
    values = arm_values(arm)
    batch_id = f"batch-{arm}"
    apply_id = f"apply-{arm}"
    apply_digest = f"apply-digest-{arm}"
    if arm in ("custody", "terminal-fresh"):
        insert_custody_state(
            db, custody_id=f"old-custody-{arm}", revision=2,
            batch_id=batch_id, apply_id=apply_id, apply_digest=apply_digest,
            head=custody_head,
        )
    elif arm == "loss":
        insert_loss_state(
            db, loss_id="terminal-loss-loss", revision=2,
            batch_id=batch_id, apply_id=apply_id, apply_digest=apply_digest,
            head=loss_head,
        )
    elif arm == "retirement" and retirement_result:
        db.execute(
            "INSERT INTO agent_lifecycle_recovery_retirements "
            "VALUES (?,?,?,?,?)",
            (
                "retirement-retirement", batch_id,
                "retirement-effect-retirement", apply_id, apply_digest,
            ),
        )

    if values["handoff_id"]:
        insert_custody_state(
            db, custody_id=str(values["new_custody"]), revision=1,
            batch_id=None, apply_id=apply_id, apply_digest=apply_digest,
            head=fresh_head,
        )
        if arm == "fresh-loss":
            insert_loss_state(
                db, loss_id=str(values["affected_loss"]), revision=2,
                batch_id=None, apply_id=apply_id, apply_digest=apply_digest,
                head=loss_head,
            )
        if fresh_commit:
            db.execute(
                "INSERT INTO lifecycle_fresh_rotation_commits "
                "VALUES (?,?,?,?,?,?)",
                (
                    f"commit-{arm}", values["handoff_id"], apply_id,
                    apply_digest, values["new_custody"],
                    values["affected_loss"],
                ),
            )


def insert_apply(
    db: sqlite3.Connection,
    arm: str,
    *,
    crossed_plain_terminal: bool = False,
) -> None:
    values = arm_values(arm)
    apply_kind = "terminal" if crossed_plain_terminal else values["apply_kind"]
    handoff_id = None if crossed_plain_terminal else values["handoff_id"]
    source_mode = None if crossed_plain_terminal else values["source_mode"]
    fresh_plan = None if crossed_plain_terminal else values["fresh_plan"]
    new_custody = None if crossed_plain_terminal else values["new_custody"]
    affected_loss = None if crossed_plain_terminal else values["affected_loss"]
    db.execute(
        "INSERT INTO lifecycle_transition_applies VALUES (?,?,?,?,?,?,?,?,?,?,?)",
        (
            f"apply-{arm}", f"apply-digest-{arm}", f"batch-{arm}",
            values["transition"], apply_kind, values["mutation"], handoff_id,
            source_mode, fresh_plan, new_custody, affected_loss,
        ),
    )


class TransitionLead456AfterTests(unittest.TestCase):
    def test_l4_crossed_terminal_fresh_batch_plain_terminal_apply_rejected(
        self,
    ) -> None:
        db = database()
        prepare_arm(db, "terminal-fresh")
        db.execute("BEGIN")
        materialize_arm(db, "terminal-fresh", fresh_head=False,
                        fresh_commit=False)
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            insert_apply(db, "terminal-fresh", crossed_plain_terminal=True)
        self.assertEqual(str(caught.exception), "FOREIGN KEY constraint failed")
        db.rollback()

    def test_l4_terminal_fresh_terminal_and_fresh_plans_stay_distinct(
        self,
    ) -> None:
        db = database()
        prepare_arm(db, "terminal-fresh")
        plans = db.execute(
            "SELECT mutation_plan_digest,fresh_apply_plan_digest "
            "FROM lifecycle_receipt_batches WHERE batch_id=?",
            ("batch-terminal-fresh",),
        ).fetchone()
        self.assertIsNotNone(plans)
        self.assertNotEqual(*plans)
        db.execute("BEGIN")
        materialize_arm(db, "terminal-fresh")
        insert_apply(db, "terminal-fresh")
        db.commit()

    def test_l5_wrong_owner_kind_rejected(self) -> None:
        db = database()
        prepare_arm(db, "custody")
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            db.execute(
                "INSERT INTO lifecycle_receipt_intents "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                (
                    "batch-custody", 1, "custody-terminal",
                    "generation-loss", "old-custody-custody", 2,
                    "custody-effect-custody", None, None, None, None,
                ),
            )
        self.assertEqual(
            str(caught.exception),
            """CHECK constraint failed: (kind IN ('custody-terminal','review-adoption-decision') AND
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
      custody_effect_digest IS NULL AND
      generation_loss_effect_role IS NULL AND
      generation_loss_effect_digest IS NULL AND
      recovery_retirement_effect_digest IS NULL AND
      fresh_origin_effect_digest IS NOT NULL)""",
        )

    def test_l5_cross_batch_effect_binding_rejected(self) -> None:
        db = database()
        prepare_arm(db, "custody")
        db.execute(
            "INSERT INTO lifecycle_receipt_batches VALUES "
            "(?,?,?,?,?,?,?,?,?,?)",
            (
                "batch-custody-b", "apply-custody-b", "custody-terminal",
                "terminal", "terminal-plan-custody-b", None, None, None,
                None, None,
            ),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_custody_effects VALUES (?,?,?,?)",
            ("batch-custody-b", "custody-effect-b", "custody-b", 2),
        )
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            db.execute(
                "INSERT INTO lifecycle_receipt_intents "
                "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                (
                    "batch-custody", 1, "custody-terminal", "custody",
                    "custody-b", 2, "custody-effect-b", None, None, None,
                    None,
                ),
            )
        self.assertEqual(str(caught.exception), "FOREIGN KEY constraint failed")

    def test_l5_extra_effect_before_completion_rejected(self) -> None:
        db = database()
        values = arm_values("custody")
        db.execute(
            "INSERT INTO lifecycle_receipt_batches VALUES (?,?,?,?,?,?,?,?,?,?)",
            (
                "batch-custody", "apply-custody", values["transition"],
                values["apply_kind"], values["mutation"], None, None, None,
                None, None,
            ),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_custody_effects VALUES (?,?,?,?)",
            (
                "batch-custody", "custody-effect-custody",
                "old-custody-custody", 2,
            ),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_generation_loss_effects "
            "VALUES (?,?,?,?,?)",
            ("batch-custody", "linked", "extra-loss", "loss-extra", 2),
        )
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            db.execute(
                "INSERT INTO lifecycle_receipt_batch_completions "
                "VALUES (?,?,?,?,?,?,?,?)",
                (
                    "batch-custody", "custody-terminal", "terminal",
                    "custody-effect-custody", None, None, None, None,
                ),
            )
        self.assertEqual(
            str(caught.exception),
            "lifecycle-effect-set-incomplete",
        )

    def test_l5_each_effect_table_closed_after_completion(self) -> None:
        inserts = (
            (
                "custody",
                "INSERT INTO lifecycle_receipt_custody_effects VALUES "
                "('batch-custody','extra-custody','other-custody',2)",
            ),
            (
                "loss",
                "INSERT INTO lifecycle_receipt_generation_loss_effects "
                "VALUES ('batch-custody','linked','extra-loss','other-loss',2)",
            ),
            (
                "retirement",
                "INSERT INTO lifecycle_receipt_recovery_retirement_effects "
                "VALUES ('batch-custody','extra-retirement','other-retirement',1)",
            ),
            (
                "fresh",
                "INSERT INTO lifecycle_receipt_fresh_origin_effects "
                "VALUES ('batch-custody','extra-fresh','other-fresh',1,NULL)",
            ),
        )
        for name, statement in inserts:
            with self.subTest(effect_table=name):
                db = database()
                prepare_arm(db, "custody")
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    db.execute(statement)
                self.assertEqual(
                    str(caught.exception),
                    "lifecycle-effect-set-closed",
                )

    def test_l6_bare_apply_rejected(self) -> None:
        db = database()
        prepare_arm(db, "custody")
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            insert_apply(db, "custody")
        self.assertEqual(
            str(caught.exception),
            "lifecycle-apply-post-state-incomplete",
        )

    def test_l6_all_six_legal_apply_arms_accept_children_first(self) -> None:
        for arm in ARMS:
            with self.subTest(arm=arm):
                db = database()
                prepare_arm(db, arm)
                db.execute("BEGIN")
                materialize_arm(db, arm)
                insert_apply(db, arm)
                db.commit()
                self.assertEqual(
                    db.execute(
                        "SELECT apply_kind FROM lifecycle_transition_applies"
                    ).fetchone(),
                    (arm_values(arm)["apply_kind"],),
                )

    def test_l6_each_arm_rejects_its_missing_head_or_result(self) -> None:
        cases = (
            ("custody", {"custody_head": False}),
            ("loss", {"loss_head": False}),
            ("retirement", {"retirement_result": False}),
            ("terminal-fresh", {"custody_head": False}),
            ("terminal-fresh", {"fresh_head": False}),
            ("fresh-reuse", {"fresh_head": False}),
            ("fresh-loss", {"fresh_head": False}),
            ("fresh-loss", {"loss_head": False}),
        )
        for index, (arm, omissions) in enumerate(cases):
            with self.subTest(index=index, arm=arm, omissions=omissions):
                db = database()
                prepare_arm(db, arm)
                db.execute("BEGIN")
                materialize_arm(db, arm, **omissions)
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    insert_apply(db, arm)
                self.assertEqual(
                    str(caught.exception),
                    "lifecycle-apply-post-state-incomplete",
                )
                db.rollback()

    def test_l6_each_fresh_arm_rejects_missing_commit(self) -> None:
        for arm in ("terminal-fresh", "fresh-reuse", "fresh-loss"):
            with self.subTest(arm=arm):
                db = database()
                prepare_arm(db, arm)
                db.execute("BEGIN")
                materialize_arm(db, arm, fresh_commit=False)
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    insert_apply(db, arm)
                self.assertEqual(
                    str(caught.exception),
                    "lifecycle-apply-post-state-incomplete",
                )
                db.rollback()


if __name__ == "__main__":
    unittest.main(verbosity=2)
