#!/usr/bin/env python3
"""Isolated executable oracle for the transition-core v2 relational design.

This intentionally transcribes only the keys, checks, deferrals, and trigger
predicates needed to prove the repaired preparation/apply graph. It is not a
substitute for the complete normative DDL in the hardening specifications.
"""

from __future__ import annotations

import sqlite3
import unittest

from assert_fk import ForeignKeySpec, assert_fk_rejected


SCHEMA = r"""
CREATE TABLE lifecycle_review_adoption_reservations(
  reservation_id TEXT PRIMARY KEY,
  reservation_digest TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  recovery_source_kind TEXT NOT NULL
    CHECK(recovery_source_kind IN ('none','custody','generation-loss')),
  decision_loss_effect_key TEXT NOT NULL,
  decision_loss_after_id TEXT,
  decision_loss_after_revision INTEGER,
  decision_loss_after_semantic_digest TEXT,
  decision_loss_after_source_ref_digest TEXT,
  UNIQUE(reservation_id,reservation_digest,decision_loss_effect_key),
  UNIQUE(reservation_id,reservation_digest,decision_loss_effect_key,
    decision_loss_after_id,decision_loss_after_revision,
    decision_loss_after_semantic_digest,
    decision_loss_after_source_ref_digest),
  CHECK(
    (recovery_source_kind IN ('none','custody') AND
      decision_loss_effect_key='none' AND
      decision_loss_after_id IS NULL AND
      decision_loss_after_revision IS NULL AND
      decision_loss_after_semantic_digest IS NULL AND
      decision_loss_after_source_ref_digest IS NULL) OR
    (recovery_source_kind='generation-loss' AND
      decision_loss_effect_key<>'none' AND
      decision_loss_after_id IS NOT NULL AND
      decision_loss_after_revision IS NOT NULL AND
      decision_loss_after_semantic_digest IS NOT NULL AND
      decision_loss_after_source_ref_digest IS NOT NULL))
);

CREATE TABLE lifecycle_receipt_batches(
  batch_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  planned_apply_id TEXT NOT NULL,
  transition_kind TEXT NOT NULL CHECK(transition_kind IN
    ('custody-terminal','generation-loss-terminal',
     'custody-recovery-retirement')),
  planned_apply_kind TEXT NOT NULL
    CHECK(planned_apply_kind IN ('terminal','terminal-fresh')),
  transition_replay_digest TEXT NOT NULL,
  mutation_plan_digest TEXT NOT NULL,
  fresh_handoff_id TEXT,
  fresh_handoff_digest TEXT,
  fresh_handoff_key TEXT NOT NULL,
  review_adoption_reservation_id TEXT,
  review_adoption_reservation_digest TEXT,
  review_decision_loss_effect_key TEXT NOT NULL,
  review_decision_loss_effect_role TEXT,
  review_decision_loss_effect_digest TEXT,
  review_decision_loss_after_id TEXT,
  review_decision_loss_after_revision INTEGER,
  review_decision_loss_after_semantic_digest TEXT,
  review_decision_loss_after_source_ref_digest TEXT,
  UNIQUE(batch_id,planned_apply_id,transition_kind,planned_apply_kind,
    transition_replay_digest,mutation_plan_digest,fresh_handoff_key),
  UNIQUE(batch_id,review_decision_loss_effect_role,
    review_decision_loss_effect_digest),
  UNIQUE(batch_id,review_decision_loss_effect_key),
  UNIQUE(batch_id,review_decision_loss_effect_role,
    review_decision_loss_effect_digest,project_session_id,run_id,agent_id,
    review_decision_loss_after_id,review_decision_loss_after_revision,
    review_decision_loss_after_semantic_digest,
    review_decision_loss_after_source_ref_digest),
  UNIQUE(batch_id,review_decision_loss_effect_key,
    review_decision_loss_effect_role,review_decision_loss_effect_digest,
    project_session_id,run_id,agent_id,review_decision_loss_after_id,
    review_decision_loss_after_revision,
    review_decision_loss_after_semantic_digest,
    review_decision_loss_after_source_ref_digest),
  CHECK(
    (planned_apply_kind='terminal' AND fresh_handoff_id IS NULL AND
      fresh_handoff_digest IS NULL AND fresh_handoff_key='none') OR
    (planned_apply_kind='terminal-fresh' AND
      transition_kind='custody-terminal' AND
      fresh_handoff_id IS NOT NULL AND fresh_handoff_digest IS NOT NULL AND
      fresh_handoff_key=fresh_handoff_digest)),
  CHECK(
    (review_adoption_reservation_id IS NULL AND
      review_adoption_reservation_digest IS NULL AND
      review_decision_loss_effect_key='none' AND
      review_decision_loss_effect_role IS NULL AND
      review_decision_loss_effect_digest IS NULL AND
      review_decision_loss_after_id IS NULL AND
      review_decision_loss_after_revision IS NULL AND
      review_decision_loss_after_semantic_digest IS NULL AND
      review_decision_loss_after_source_ref_digest IS NULL) OR
    (review_adoption_reservation_id IS NOT NULL AND
      review_adoption_reservation_digest IS NOT NULL AND
      review_decision_loss_effect_key='none' AND
      review_decision_loss_effect_role IS NULL AND
      review_decision_loss_effect_digest IS NULL AND
      review_decision_loss_after_id IS NULL AND
      review_decision_loss_after_revision IS NULL AND
      review_decision_loss_after_semantic_digest IS NULL AND
      review_decision_loss_after_source_ref_digest IS NULL) OR
    (review_adoption_reservation_id IS NOT NULL AND
      review_adoption_reservation_digest IS NOT NULL AND
      review_decision_loss_effect_key<>'none' AND
      review_decision_loss_effect_role IS NOT NULL AND
      review_decision_loss_effect_digest IS NOT NULL AND
      review_decision_loss_effect_role='linked' AND
      review_decision_loss_effect_digest=review_decision_loss_effect_key AND
      review_decision_loss_after_id IS NOT NULL AND
      review_decision_loss_after_revision IS NOT NULL AND
      review_decision_loss_after_semantic_digest IS NOT NULL AND
      review_decision_loss_after_source_ref_digest IS NOT NULL)),
  FOREIGN KEY(review_adoption_reservation_id,
      review_adoption_reservation_digest,review_decision_loss_effect_key)
    REFERENCES lifecycle_review_adoption_reservations(
      reservation_id,reservation_digest,decision_loss_effect_key),
  FOREIGN KEY(review_adoption_reservation_id,
      review_adoption_reservation_digest,review_decision_loss_effect_key,
      review_decision_loss_after_id,review_decision_loss_after_revision,
      review_decision_loss_after_semantic_digest,
      review_decision_loss_after_source_ref_digest)
    REFERENCES lifecycle_review_adoption_reservations(
      reservation_id,reservation_digest,decision_loss_effect_key,
      decision_loss_after_id,decision_loss_after_revision,
      decision_loss_after_semantic_digest,
      decision_loss_after_source_ref_digest),
  FOREIGN KEY(batch_id,review_decision_loss_effect_role,
      review_decision_loss_effect_digest,project_session_id,run_id,agent_id,
      review_decision_loss_after_id,review_decision_loss_after_revision,
      review_decision_loss_after_semantic_digest,
      review_decision_loss_after_source_ref_digest)
    REFERENCES lifecycle_receipt_generation_loss_effects(
      batch_id,role,effect_digest,project_session_id,run_id,agent_id,
      generation_loss_id,final_revision,final_semantic_digest,
      final_source_ref_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE lifecycle_receipt_generation_loss_effects(
  batch_id TEXT NOT NULL,
  role TEXT NOT NULL CHECK(role IN ('primary','linked')),
  effect_digest TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  generation_loss_id TEXT NOT NULL,
  pre_revision INTEGER NOT NULL,
  final_revision INTEGER NOT NULL,
  final_semantic_digest TEXT NOT NULL,
  final_source_ref_digest TEXT NOT NULL,
  PRIMARY KEY(batch_id,role,effect_digest),
  UNIQUE(batch_id,role),
  UNIQUE(batch_id,role,effect_digest,project_session_id,run_id,agent_id,
    generation_loss_id,final_revision,final_semantic_digest,
    final_source_ref_digest),
  FOREIGN KEY(batch_id) REFERENCES lifecycle_receipt_batches(batch_id)
);

CREATE TABLE lifecycle_receipt_batch_completions(
  batch_id TEXT PRIMARY KEY,
  linked_loss_effect_role TEXT,
  linked_loss_effect_digest TEXT,
  CHECK((linked_loss_effect_role IS NULL AND
          linked_loss_effect_digest IS NULL) OR
        (linked_loss_effect_role='linked' AND
          linked_loss_effect_digest IS NOT NULL)),
  FOREIGN KEY(batch_id) REFERENCES lifecycle_receipt_batches(batch_id),
  FOREIGN KEY(batch_id,linked_loss_effect_role,linked_loss_effect_digest)
    REFERENCES lifecycle_receipt_generation_loss_effects(
      batch_id,role,effect_digest)
);

CREATE TABLE lifecycle_fresh_recovery_handoffs(
  handoff_id TEXT PRIMARY KEY,
  handoff_digest TEXT NOT NULL,
  planned_apply_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  new_custody_id TEXT NOT NULL,
  new_custody_semantic_digest TEXT NOT NULL,
  new_custody_source_ref_digest TEXT NOT NULL,
  fresh_apply_plan_digest TEXT NOT NULL,
  affected_generation_loss_id TEXT,
  affected_generation_loss_after_revision INTEGER,
  affected_generation_loss_after_semantic_digest TEXT,
  affected_generation_loss_after_source_ref_digest TEXT,
  affected_generation_loss_after_key TEXT NOT NULL,
  CHECK(
    (affected_generation_loss_id IS NULL AND
      affected_generation_loss_after_revision IS NULL AND
      affected_generation_loss_after_semantic_digest IS NULL AND
      affected_generation_loss_after_source_ref_digest IS NULL AND
      affected_generation_loss_after_key='none') OR
    (affected_generation_loss_id IS NOT NULL AND
      affected_generation_loss_after_revision IS NOT NULL AND
      affected_generation_loss_after_semantic_digest IS NOT NULL AND
      affected_generation_loss_after_source_ref_digest IS NOT NULL AND
      affected_generation_loss_after_key=
        affected_generation_loss_after_source_ref_digest)),
  UNIQUE(handoff_id,handoff_digest,affected_generation_loss_after_key),
  UNIQUE(handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
    agent_id,new_custody_id,new_custody_semantic_digest,
    new_custody_source_ref_digest,fresh_apply_plan_digest,
    affected_generation_loss_after_key),
  UNIQUE(handoff_id,planned_apply_id,affected_generation_loss_id,
    affected_generation_loss_after_revision,
    affected_generation_loss_after_semantic_digest,
    affected_generation_loss_after_source_ref_digest),
  UNIQUE(handoff_id,handoff_digest,affected_generation_loss_after_key,
    affected_generation_loss_id,
    affected_generation_loss_after_revision,
    affected_generation_loss_after_semantic_digest,
    affected_generation_loss_after_source_ref_digest)
);

CREATE TABLE lifecycle_transition_applies(
  apply_id TEXT PRIMARY KEY,
  apply_digest TEXT NOT NULL,
  receipt_batch_id TEXT NOT NULL,
  batch_transition_kind TEXT NOT NULL,
  apply_kind TEXT NOT NULL CHECK(apply_kind IN ('terminal','terminal-fresh')),
  transition_replay_digest TEXT NOT NULL,
  applied_mutation_plan_digest TEXT NOT NULL,
  fresh_handoff_id TEXT,
  fresh_handoff_digest TEXT,
  fresh_handoff_key TEXT NOT NULL,
  fresh_project_session_id TEXT,
  fresh_run_id TEXT,
  fresh_agent_id TEXT,
  new_custody_id TEXT,
  new_custody_semantic_digest TEXT,
  new_custody_source_ref_digest TEXT,
  fresh_apply_plan_digest TEXT,
  fresh_generation_loss_id TEXT,
  fresh_generation_loss_after_revision INTEGER,
  fresh_generation_loss_after_semantic_digest TEXT,
  fresh_generation_loss_after_source_ref_digest TEXT,
  fresh_generation_loss_after_key TEXT NOT NULL,
  UNIQUE(apply_id,apply_digest),
  UNIQUE(apply_id,receipt_batch_id),
  UNIQUE(apply_id,apply_digest,fresh_generation_loss_after_key),
  UNIQUE(apply_id,apply_digest,fresh_generation_loss_after_key,
    fresh_generation_loss_id,fresh_generation_loss_after_revision,
    fresh_generation_loss_after_semantic_digest,
    fresh_generation_loss_after_source_ref_digest),
  CHECK(
    (apply_kind='terminal' AND fresh_handoff_id IS NULL AND
      fresh_handoff_digest IS NULL AND fresh_handoff_key='none' AND
      fresh_project_session_id IS NULL AND fresh_run_id IS NULL AND
      fresh_agent_id IS NULL AND new_custody_id IS NULL AND
      new_custody_semantic_digest IS NULL AND
      new_custody_source_ref_digest IS NULL AND
      fresh_apply_plan_digest IS NULL AND
      fresh_generation_loss_id IS NULL AND
      fresh_generation_loss_after_revision IS NULL AND
      fresh_generation_loss_after_semantic_digest IS NULL AND
      fresh_generation_loss_after_source_ref_digest IS NULL AND
      fresh_generation_loss_after_key='none') OR
    (apply_kind='terminal-fresh' AND
      batch_transition_kind='custody-terminal' AND
      fresh_handoff_id IS NOT NULL AND fresh_handoff_digest IS NOT NULL AND
      fresh_handoff_key=fresh_handoff_digest AND
      fresh_project_session_id IS NOT NULL AND fresh_run_id IS NOT NULL AND
      fresh_agent_id IS NOT NULL AND new_custody_id IS NOT NULL AND
      new_custody_semantic_digest IS NOT NULL AND
      new_custody_source_ref_digest IS NOT NULL AND
      fresh_apply_plan_digest IS NOT NULL AND
      ((fresh_generation_loss_id IS NULL AND
        fresh_generation_loss_after_revision IS NULL AND
        fresh_generation_loss_after_semantic_digest IS NULL AND
        fresh_generation_loss_after_source_ref_digest IS NULL AND
        fresh_generation_loss_after_key='none') OR
       (fresh_generation_loss_id IS NOT NULL AND
        fresh_generation_loss_after_revision IS NOT NULL AND
        fresh_generation_loss_after_semantic_digest IS NOT NULL AND
        fresh_generation_loss_after_source_ref_digest IS NOT NULL AND
        fresh_generation_loss_after_key=
          fresh_generation_loss_after_source_ref_digest)))),
  FOREIGN KEY(receipt_batch_id,apply_id,batch_transition_kind,apply_kind,
      transition_replay_digest,applied_mutation_plan_digest,fresh_handoff_key)
    REFERENCES lifecycle_receipt_batches(
      batch_id,planned_apply_id,transition_kind,planned_apply_kind,
      transition_replay_digest,mutation_plan_digest,fresh_handoff_key),
  FOREIGN KEY(fresh_handoff_id,fresh_handoff_digest,apply_id,
      fresh_project_session_id,fresh_run_id,fresh_agent_id,new_custody_id,
      new_custody_semantic_digest,new_custody_source_ref_digest,
      fresh_apply_plan_digest,fresh_generation_loss_after_key)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
      agent_id,new_custody_id,new_custody_semantic_digest,
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
      affected_generation_loss_after_source_ref_digest)
);

CREATE TABLE lifecycle_generation_loss_revisions(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  generation_loss_id TEXT NOT NULL,
  revision INTEGER NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  receipt_batch_id TEXT NOT NULL,
  receipt_apply_id TEXT NOT NULL,
  receipt_apply_digest TEXT NOT NULL,
  PRIMARY KEY(project_session_id,run_id,agent_id,generation_loss_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
    semantic_digest,source_ref_digest,journal_digest),
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest)
    REFERENCES lifecycle_transition_applies(apply_id,apply_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE lifecycle_generation_loss_heads(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  generation_loss_id TEXT NOT NULL,
  current_revision INTEGER NOT NULL,
  semantic_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  PRIMARY KEY(project_session_id,run_id,agent_id,generation_loss_id),
  FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
      current_revision,semantic_digest,source_ref_digest,journal_digest)
    REFERENCES lifecycle_generation_loss_revisions(
      project_session_id,run_id,agent_id,generation_loss_id,revision,
      semantic_digest,source_ref_digest,journal_digest)
);

CREATE TABLE lifecycle_fresh_rotation_commits(
  commit_id TEXT PRIMARY KEY,
  handoff_id TEXT NOT NULL,
  handoff_digest TEXT NOT NULL,
  apply_id TEXT NOT NULL,
  fresh_apply_digest TEXT NOT NULL,
  generation_loss_after_id TEXT,
  generation_loss_after_revision INTEGER,
  generation_loss_after_semantic_digest TEXT,
  generation_loss_after_source_ref_digest TEXT,
  generation_loss_after_journal_digest TEXT,
  generation_loss_after_key TEXT NOT NULL,
  CHECK(
    (generation_loss_after_id IS NULL AND
      generation_loss_after_revision IS NULL AND
      generation_loss_after_semantic_digest IS NULL AND
      generation_loss_after_source_ref_digest IS NULL AND
      generation_loss_after_journal_digest IS NULL AND
      generation_loss_after_key='none') OR
    (generation_loss_after_id IS NOT NULL AND
      generation_loss_after_revision IS NOT NULL AND
      generation_loss_after_semantic_digest IS NOT NULL AND
      generation_loss_after_source_ref_digest IS NOT NULL AND
      generation_loss_after_journal_digest IS NOT NULL AND
      generation_loss_after_key=generation_loss_after_source_ref_digest)),
  FOREIGN KEY(handoff_id,handoff_digest,generation_loss_after_key)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,affected_generation_loss_after_key),
  FOREIGN KEY(handoff_id,handoff_digest,generation_loss_after_key,
      generation_loss_after_id,generation_loss_after_revision,
      generation_loss_after_semantic_digest,
      generation_loss_after_source_ref_digest)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,affected_generation_loss_after_key,
      affected_generation_loss_id,
      affected_generation_loss_after_revision,
      affected_generation_loss_after_semantic_digest,
      affected_generation_loss_after_source_ref_digest),
  FOREIGN KEY(apply_id,fresh_apply_digest,generation_loss_after_key)
    REFERENCES lifecycle_transition_applies(
      apply_id,apply_digest,fresh_generation_loss_after_key)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(apply_id,fresh_apply_digest,generation_loss_after_key,
      generation_loss_after_id,generation_loss_after_revision,
      generation_loss_after_semantic_digest,
      generation_loss_after_source_ref_digest)
    REFERENCES lifecycle_transition_applies(
      apply_id,apply_digest,fresh_generation_loss_after_key,
      fresh_generation_loss_id,fresh_generation_loss_after_revision,
      fresh_generation_loss_after_semantic_digest,
      fresh_generation_loss_after_source_ref_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE lifecycle_review_authority_bindings(
  binding_id TEXT PRIMARY KEY,
  batch_id TEXT NOT NULL,
  apply_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  decision_loss_effect_key TEXT NOT NULL,
  decision_loss_effect_role TEXT,
  decision_loss_effect_digest TEXT,
  decision_loss_after_id TEXT,
  decision_loss_after_revision INTEGER,
  decision_loss_after_semantic_digest TEXT,
  decision_loss_after_source_ref_digest TEXT,
  CHECK(
    (decision_loss_effect_key='none' AND
      decision_loss_effect_role IS NULL AND
      decision_loss_effect_digest IS NULL AND
      decision_loss_after_id IS NULL AND
      decision_loss_after_revision IS NULL AND
      decision_loss_after_semantic_digest IS NULL AND
      decision_loss_after_source_ref_digest IS NULL) OR
    (decision_loss_effect_key<>'none' AND
      decision_loss_effect_role IS NOT NULL AND
      decision_loss_effect_digest IS NOT NULL AND
      decision_loss_effect_role='linked' AND
      decision_loss_effect_digest=decision_loss_effect_key AND
      decision_loss_after_id IS NOT NULL AND
      decision_loss_after_revision IS NOT NULL AND
      decision_loss_after_semantic_digest IS NOT NULL AND
      decision_loss_after_source_ref_digest IS NOT NULL)),
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
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE lifecycle_rotation_custody_revisions(
  custody_revision_id TEXT PRIMARY KEY,
  receipt_apply_id TEXT NOT NULL,
  receipt_apply_digest TEXT NOT NULL,
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest)
    REFERENCES lifecycle_transition_applies(apply_id,apply_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TABLE agent_lifecycle_recovery_retirements(
  retirement_id TEXT PRIMARY KEY,
  receipt_apply_id TEXT NOT NULL,
  receipt_apply_digest TEXT NOT NULL,
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest)
    REFERENCES lifecycle_transition_applies(apply_id,apply_digest)
    DEFERRABLE INITIALLY DEFERRED
);

CREATE TRIGGER lifecycle_terminal_fresh_linked_loss
BEFORE INSERT ON lifecycle_transition_applies
WHEN NEW.apply_kind='terminal-fresh'
BEGIN
  SELECT RAISE(ABORT,'lifecycle-terminal-fresh-linked-loss-crossed')
  WHERE NOT (
    (
      NEW.fresh_generation_loss_after_key='none' AND
      NEW.fresh_generation_loss_id IS NULL AND
      NEW.fresh_generation_loss_after_revision IS NULL AND
      NEW.fresh_generation_loss_after_semantic_digest IS NULL AND
      NEW.fresh_generation_loss_after_source_ref_digest IS NULL AND
      EXISTS (
        SELECT 1 FROM lifecycle_receipt_batch_completions c
        WHERE c.batch_id=NEW.receipt_batch_id
          AND c.linked_loss_effect_role IS NULL
          AND c.linked_loss_effect_digest IS NULL
      ) AND
      NOT EXISTS (
        SELECT 1 FROM lifecycle_receipt_generation_loss_effects e
        WHERE e.batch_id=NEW.receipt_batch_id AND e.role='linked'
      )
    ) OR (
      NEW.fresh_generation_loss_after_key<>'none' AND
      NEW.fresh_generation_loss_after_key=
        NEW.fresh_generation_loss_after_source_ref_digest AND
      EXISTS (
        SELECT 1
        FROM lifecycle_receipt_batch_completions c
        JOIN lifecycle_receipt_generation_loss_effects e
          ON e.batch_id=c.batch_id
         AND e.role='linked'
         AND e.effect_digest=c.linked_loss_effect_digest
        JOIN lifecycle_generation_loss_revisions r
          ON r.project_session_id=e.project_session_id
         AND r.run_id=e.run_id AND r.agent_id=e.agent_id
         AND r.generation_loss_id=e.generation_loss_id
         AND r.revision=e.final_revision
         AND r.semantic_digest=e.final_semantic_digest
         AND r.source_ref_digest=e.final_source_ref_digest
         AND r.receipt_batch_id=e.batch_id
         AND r.receipt_apply_id=NEW.apply_id
         AND r.receipt_apply_digest=NEW.apply_digest
        JOIN lifecycle_generation_loss_heads h
          ON h.project_session_id=r.project_session_id
         AND h.run_id=r.run_id AND h.agent_id=r.agent_id
         AND h.generation_loss_id=r.generation_loss_id
         AND h.current_revision=r.revision
         AND h.semantic_digest=r.semantic_digest
         AND h.source_ref_digest=r.source_ref_digest
         AND h.journal_digest=r.journal_digest
        WHERE c.batch_id=NEW.receipt_batch_id
          AND c.linked_loss_effect_digest IS NOT NULL
          AND e.project_session_id=NEW.fresh_project_session_id
          AND e.run_id=NEW.fresh_run_id
          AND e.agent_id=NEW.fresh_agent_id
          AND e.generation_loss_id=NEW.fresh_generation_loss_id
          AND e.final_revision=NEW.fresh_generation_loss_after_revision
          AND e.final_semantic_digest=
            NEW.fresh_generation_loss_after_semantic_digest
          AND e.final_source_ref_digest=
            NEW.fresh_generation_loss_after_source_ref_digest
      )
    )
  );
END;

CREATE TRIGGER lifecycle_terminal_fresh_commit_required
BEFORE INSERT ON lifecycle_transition_applies
WHEN NEW.apply_kind='terminal-fresh'
BEGIN
  SELECT RAISE(ABORT,'lifecycle-apply-post-state-incomplete')
  WHERE NOT EXISTS (
    SELECT 1 FROM lifecycle_fresh_rotation_commits c
    WHERE c.handoff_id=NEW.fresh_handoff_id
      AND c.handoff_digest=NEW.fresh_handoff_digest
      AND c.apply_id=NEW.apply_id
      AND c.fresh_apply_digest=NEW.apply_digest
      AND c.generation_loss_after_key=
        NEW.fresh_generation_loss_after_key
      AND c.generation_loss_after_id IS
        NEW.fresh_generation_loss_id
      AND c.generation_loss_after_revision IS
        NEW.fresh_generation_loss_after_revision
      AND c.generation_loss_after_semantic_digest IS
        NEW.fresh_generation_loss_after_semantic_digest
      AND c.generation_loss_after_source_ref_digest IS
        NEW.fresh_generation_loss_after_source_ref_digest
  );
END;
"""


SCOPE = ("session-1", "run-1", "agent-1")
LOSS = ("loss-1", 2, "semantic-2", "source-2", "journal-2")


def database() -> sqlite3.Connection:
    db = sqlite3.connect(":memory:", isolation_level=None)
    db.execute("PRAGMA foreign_keys=ON")
    db.executescript(SCHEMA)
    return db


def insert_review_reservation(db: sqlite3.Connection) -> None:
    db.execute(
        """INSERT INTO lifecycle_review_adoption_reservations VALUES
        (?,?,?,?,?,'generation-loss',?,?,?,?,?)""",
        ("reservation-1", "reservation-digest", *SCOPE,
         "effect-review", LOSS[0], LOSS[1], LOSS[2], LOSS[3]),
    )


def insert_batch(
    db: sqlite3.Connection,
    *,
    batch_id: str,
    apply_id: str,
    transition_kind: str = "custody-terminal",
    apply_kind: str = "terminal",
    replay: str | None = None,
    mutation: str | None = None,
    handoff_id: str | None = None,
    handoff_digest: str | None = None,
    review: bool = False,
) -> None:
    replay = replay or f"replay-{batch_id}"
    mutation = mutation or f"mutation-{batch_id}"
    fresh_key = handoff_digest if apply_kind == "terminal-fresh" else "none"
    if review:
        review_values = (
            "reservation-1", "reservation-digest", "effect-review",
            "linked", "effect-review", LOSS[0], LOSS[1], LOSS[2], LOSS[3],
        )
    else:
        review_values = (None, None, "none", None, None, None, None, None, None)
    db.execute(
        """INSERT INTO lifecycle_receipt_batches VALUES
        (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
        (batch_id, *SCOPE, apply_id, transition_kind, apply_kind, replay,
         mutation, handoff_id, handoff_digest, fresh_key, *review_values),
    )


def insert_review_effect(
    db: sqlite3.Connection,
    *,
    final_revision: int = LOSS[1],
    final_semantic: str = LOSS[2],
    final_source: str = LOSS[3],
) -> None:
    db.execute(
        """INSERT INTO lifecycle_receipt_generation_loss_effects VALUES
        (?,?,?, ?,?,?, ?,?,?,?,?)""",
        ("batch-review", "linked", "effect-review", *SCOPE, LOSS[0], 1,
         final_revision, final_semantic, final_source),
    )


def seed_review_prepare(db: sqlite3.Connection) -> None:
    db.execute("BEGIN")
    insert_review_reservation(db)
    insert_batch(db, batch_id="batch-review", apply_id="apply-review",
                 review=True)
    insert_review_effect(db)
    db.commit()


def insert_handoff(
    db: sqlite3.Connection,
    *,
    with_loss: bool,
    handoff_id: str = "handoff-1",
    apply_id: str = "apply-fresh",
) -> None:
    loss_values = LOSS[:4] if with_loss else (None, None, None, None)
    loss_key = LOSS[3] if with_loss else "none"
    db.execute(
        """INSERT INTO lifecycle_fresh_recovery_handoffs VALUES
        (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
        (handoff_id, f"digest-{handoff_id}", apply_id, *SCOPE,
         f"custody-{handoff_id}", f"custody-semantic-{handoff_id}",
         f"custody-source-{handoff_id}", f"fresh-plan-{handoff_id}",
         *loss_values, loss_key),
    )


def seed_terminal_fresh_prepare(
    db: sqlite3.Connection,
    *,
    completion_has_loss: bool,
    handoff_has_loss: bool,
) -> None:
    db.execute("BEGIN")
    insert_handoff(db, with_loss=handoff_has_loss)
    insert_batch(
        db,
        batch_id="batch-fresh",
        apply_id="apply-fresh",
        apply_kind="terminal-fresh",
        replay="replay-fresh",
        mutation="terminal-plan",
        handoff_id="handoff-1",
        handoff_digest="digest-handoff-1",
    )
    if completion_has_loss:
        db.execute(
            """INSERT INTO lifecycle_receipt_generation_loss_effects VALUES
            (?,?,?, ?,?,?, ?,?,?,?,?)""",
            ("batch-fresh", "linked", "effect-linked", *SCOPE, LOSS[0], 1,
             LOSS[1], LOSS[2], LOSS[3]),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_batch_completions VALUES (?,?,?)",
            ("batch-fresh", "linked", "effect-linked"),
        )
    else:
        db.execute(
            "INSERT INTO lifecycle_receipt_batch_completions VALUES (?,NULL,NULL)",
            ("batch-fresh",),
        )
    db.commit()


def insert_loss_revision_and_head(db: sqlite3.Connection) -> None:
    db.execute(
        """INSERT INTO lifecycle_generation_loss_revisions VALUES
        (?,?,?,?,?,?,?,?,?,?,?)""",
        (*SCOPE, LOSS[0], LOSS[1], LOSS[2], LOSS[3], LOSS[4],
         "batch-fresh", "apply-fresh", "apply-digest-fresh"),
    )
    db.execute(
        """INSERT INTO lifecycle_generation_loss_heads VALUES
        (?,?,?,?,?,?,?,?)""",
        (*SCOPE, LOSS[0], LOSS[1], LOSS[2], LOSS[3], LOSS[4]),
    )


def insert_fresh_commit(db: sqlite3.Connection, *, with_loss: bool) -> None:
    loss_values = LOSS if with_loss else (None, None, None, None, None)
    loss_key = LOSS[3] if with_loss else "none"
    db.execute(
        """INSERT INTO lifecycle_fresh_rotation_commits VALUES
        (?,?,?,?,?,?,?,?,?,?,?)""",
        ("commit-1", "handoff-1", "digest-handoff-1", "apply-fresh",
         "apply-digest-fresh", *loss_values, loss_key),
    )


def insert_apply(
    db: sqlite3.Connection,
    *,
    batch_id: str,
    apply_id: str,
    apply_digest: str,
    transition_kind: str,
    apply_kind: str = "terminal",
    replay: str | None = None,
    mutation: str | None = None,
    handoff_has_loss: bool = False,
) -> None:
    replay = replay or f"replay-{batch_id}"
    mutation = mutation or f"mutation-{batch_id}"
    if apply_kind == "terminal-fresh":
        loss_values = LOSS[:4] if handoff_has_loss else (None, None, None, None)
        loss_key = LOSS[3] if handoff_has_loss else "none"
        fresh_values = (
            "handoff-1", "digest-handoff-1", "digest-handoff-1", *SCOPE,
            "custody-handoff-1", "custody-semantic-handoff-1",
            "custody-source-handoff-1", "fresh-plan-handoff-1",
            *loss_values, loss_key,
        )
    else:
        fresh_values = (
            None, None, "none", None, None, None, None, None, None, None,
            None, None, None, None, "none",
        )
    db.execute(
        """INSERT INTO lifecycle_transition_applies VALUES
        (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
        (apply_id, apply_digest, batch_id, transition_kind, apply_kind,
         replay, mutation, *fresh_values),
    )


def insert_review_binding(
    db: sqlite3.Connection,
    *,
    revision: int,
    binding_id: str = "binding-1",
) -> None:
    db.execute(
        """INSERT INTO lifecycle_review_authority_bindings VALUES
        (?,?,?,?,?,?,?,?,?,?,?,?,?)""",
        (binding_id, "batch-review", "apply-review", *SCOPE,
         "effect-review", "linked", "effect-review", LOSS[0],
         revision, LOSS[2], LOSS[3]),
    )


class TransitionCoreAfterTests(unittest.TestCase):
    def test_batch_side_exact_effect_closes_at_commit(self) -> None:
        db = database()
        db.execute("BEGIN")
        insert_review_reservation(db)
        insert_batch(db, batch_id="batch-review", apply_id="apply-review",
                     review=True)
        insert_review_effect(db)
        db.commit()
        self.assertEqual(
            db.execute(
                "SELECT final_revision FROM "
                "lifecycle_receipt_generation_loss_effects"
            ).fetchone(),
            (2,),
        )

    def test_batch_side_missing_effect_fails_at_commit(self) -> None:
        db = database()

        def insert_batch_without_effect(connection: sqlite3.Connection) -> None:
            insert_review_reservation(connection)
            insert_batch(
                connection,
                batch_id="batch-review",
                apply_id="apply-review",
                review=True,
            )

        def insert_batch_with_effect(connection: sqlite3.Connection) -> None:
            insert_batch_without_effect(connection)
            insert_review_effect(connection)

        assert_fk_rejected(
            db,
            invalid_operation=insert_batch_without_effect,
            positive_control=insert_batch_with_effect,
            expected=frozenset(
                {
                    ForeignKeySpec(
                        "lifecycle_receipt_batches",
                        (
                            "batch_id",
                            "review_decision_loss_effect_role",
                            "review_decision_loss_effect_digest",
                            "project_session_id",
                            "run_id",
                            "agent_id",
                            "review_decision_loss_after_id",
                            "review_decision_loss_after_revision",
                            "review_decision_loss_after_semantic_digest",
                            "review_decision_loss_after_source_ref_digest",
                        ),
                        "lifecycle_receipt_generation_loss_effects",
                        (
                            "batch_id",
                            "role",
                            "effect_digest",
                            "project_session_id",
                            "run_id",
                            "agent_id",
                            "generation_loss_id",
                            "final_revision",
                            "final_semantic_digest",
                            "final_source_ref_digest",
                        ),
                    )
                }
            ),
        )

    def test_batch_effect_tuple_cannot_null_skip_deferred_fk(self) -> None:
        db = database()
        db.execute("BEGIN")
        insert_review_reservation(db)
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            db.execute(
                """INSERT INTO lifecycle_receipt_batches VALUES
                (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)""",
                ("batch-review", *SCOPE, "apply-review",
                 "custody-terminal", "terminal", "replay-batch-review",
                 "mutation-batch-review", None, None, "none",
                 "reservation-1", "reservation-digest", "effect-review",
                 None, None, LOSS[0], LOSS[1], LOSS[2], LOSS[3]),
            )
        self.assertEqual(
            str(caught.exception),
            """CHECK constraint failed: (review_adoption_reservation_id IS NULL AND
      review_adoption_reservation_digest IS NULL AND
      review_decision_loss_effect_key='none' AND
      review_decision_loss_effect_role IS NULL AND
      review_decision_loss_effect_digest IS NULL AND
      review_decision_loss_after_id IS NULL AND
      review_decision_loss_after_revision IS NULL AND
      review_decision_loss_after_semantic_digest IS NULL AND
      review_decision_loss_after_source_ref_digest IS NULL) OR
    (review_adoption_reservation_id IS NOT NULL AND
      review_adoption_reservation_digest IS NOT NULL AND
      review_decision_loss_effect_key='none' AND
      review_decision_loss_effect_role IS NULL AND
      review_decision_loss_effect_digest IS NULL AND
      review_decision_loss_after_id IS NULL AND
      review_decision_loss_after_revision IS NULL AND
      review_decision_loss_after_semantic_digest IS NULL AND
      review_decision_loss_after_source_ref_digest IS NULL) OR
    (review_adoption_reservation_id IS NOT NULL AND
      review_adoption_reservation_digest IS NOT NULL AND
      review_decision_loss_effect_key<>'none' AND
      review_decision_loss_effect_role IS NOT NULL AND
      review_decision_loss_effect_digest IS NOT NULL AND
      review_decision_loss_effect_role='linked' AND
      review_decision_loss_effect_digest=review_decision_loss_effect_key AND
      review_decision_loss_after_id IS NOT NULL AND
      review_decision_loss_after_revision IS NOT NULL AND
      review_decision_loss_after_semantic_digest IS NOT NULL AND
      review_decision_loss_after_source_ref_digest IS NOT NULL)""",
        )
        db.rollback()

    def test_batch_side_crossed_effect_fails_at_commit(self) -> None:
        db = database()

        def insert_crossed_effect(connection: sqlite3.Connection) -> None:
            insert_review_reservation(connection)
            insert_batch(
                connection,
                batch_id="batch-review",
                apply_id="apply-review",
                review=True,
            )
            insert_review_effect(connection, final_revision=3)

        def insert_matching_effect(connection: sqlite3.Connection) -> None:
            insert_review_reservation(connection)
            insert_batch(
                connection,
                batch_id="batch-review",
                apply_id="apply-review",
                review=True,
            )
            insert_review_effect(connection)

        assert_fk_rejected(
            db,
            invalid_operation=insert_crossed_effect,
            positive_control=insert_matching_effect,
            expected=frozenset(
                {
                    ForeignKeySpec(
                        "lifecycle_receipt_batches",
                        (
                            "batch_id",
                            "review_decision_loss_effect_role",
                            "review_decision_loss_effect_digest",
                            "project_session_id",
                            "run_id",
                            "agent_id",
                            "review_decision_loss_after_id",
                            "review_decision_loss_after_revision",
                            "review_decision_loss_after_semantic_digest",
                            "review_decision_loss_after_source_ref_digest",
                        ),
                        "lifecycle_receipt_generation_loss_effects",
                        (
                            "batch_id",
                            "role",
                            "effect_digest",
                            "project_session_id",
                            "run_id",
                            "agent_id",
                            "generation_loss_id",
                            "final_revision",
                            "final_semantic_digest",
                            "final_source_ref_digest",
                        ),
                    )
                }
            ),
        )

    def test_reservation_has_no_batch_apply_or_receipt_back_pointer(self) -> None:
        db = database()
        columns = {
            row[1]
            for row in db.execute(
                "PRAGMA table_info(lifecycle_review_adoption_reservations)"
            )
        }
        self.assertFalse(
            any(token in column for column in columns
                for token in ("batch", "apply", "receipt")),
            columns,
        )

    def test_review_binding_uses_effect_final_not_pre_revision(self) -> None:
        db = database()
        seed_review_prepare(db)
        self.assertEqual(
            db.execute(
                "SELECT pre_revision,final_revision FROM "
                "lifecycle_receipt_generation_loss_effects"
            ).fetchone(),
            (1, 2),
        )
        db.execute("BEGIN")
        insert_review_binding(db, revision=2)
        insert_apply(
            db, batch_id="batch-review", apply_id="apply-review",
            apply_digest="apply-digest-review",
            transition_kind="custody-terminal",
        )
        db.commit()

        crossed = database()
        seed_review_prepare(crossed)
        crossed.execute("BEGIN")
        insert_apply(
            crossed,
            batch_id="batch-review",
            apply_id="apply-review",
            apply_digest="apply-digest-review",
            transition_kind="custody-terminal",
        )
        crossed.commit()
        assert_fk_rejected(
            crossed,
            invalid_operation=lambda connection: insert_review_binding(
                connection,
                revision=1,
            ),
            positive_control=lambda connection: insert_review_binding(
                connection,
                revision=2,
            ),
            expected=frozenset(
                {
                    ForeignKeySpec(
                        "lifecycle_review_authority_bindings",
                        (
                            "batch_id",
                            "decision_loss_effect_key",
                            "decision_loss_effect_role",
                            "decision_loss_effect_digest",
                            "project_session_id",
                            "run_id",
                            "agent_id",
                            "decision_loss_after_id",
                            "decision_loss_after_revision",
                            "decision_loss_after_semantic_digest",
                            "decision_loss_after_source_ref_digest",
                        ),
                        "lifecycle_receipt_batches",
                        (
                            "batch_id",
                            "review_decision_loss_effect_key",
                            "review_decision_loss_effect_role",
                            "review_decision_loss_effect_digest",
                            "project_session_id",
                            "run_id",
                            "agent_id",
                            "review_decision_loss_after_id",
                            "review_decision_loss_after_revision",
                            "review_decision_loss_after_semantic_digest",
                            "review_decision_loss_after_source_ref_digest",
                        ),
                    )
                }
            ),
        )

    def test_review_binding_missing_apply_fails_only_at_commit(self) -> None:
        db = database()
        seed_review_prepare(db)

        def binding_without_apply(connection: sqlite3.Connection) -> None:
            insert_review_binding(connection, revision=2)

        def binding_with_apply(connection: sqlite3.Connection) -> None:
            binding_without_apply(connection)
            insert_apply(
                connection,
                batch_id="batch-review",
                apply_id="apply-review",
                apply_digest="apply-digest-review",
                transition_kind="custody-terminal",
            )

        assert_fk_rejected(
            db,
            invalid_operation=binding_without_apply,
            positive_control=binding_with_apply,
            expected=frozenset(
                {
                    ForeignKeySpec(
                        "lifecycle_review_authority_bindings",
                        ("apply_id", "batch_id"),
                        "lifecycle_transition_applies",
                        ("apply_id", "receipt_batch_id"),
                    )
                }
            ),
        )

    def test_review_binding_effect_tuple_cannot_null_skip_full_fk(self) -> None:
        db = database()
        seed_review_prepare(db)
        db.execute("BEGIN")
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            db.execute(
                """INSERT INTO lifecycle_review_authority_bindings VALUES
                (?,?,?,?,?,?,?,?,?,?,?,?,?)""",
                ("binding-null-skip", "batch-review", "apply-review", *SCOPE,
                 "effect-review", None, None,
                 LOSS[0], LOSS[1], LOSS[2], LOSS[3]),
            )
        self.assertEqual(
            str(caught.exception),
            """CHECK constraint failed: (decision_loss_effect_key='none' AND
      decision_loss_effect_role IS NULL AND
      decision_loss_effect_digest IS NULL AND
      decision_loss_after_id IS NULL AND
      decision_loss_after_revision IS NULL AND
      decision_loss_after_semantic_digest IS NULL AND
      decision_loss_after_source_ref_digest IS NULL) OR
    (decision_loss_effect_key<>'none' AND
      decision_loss_effect_role IS NOT NULL AND
      decision_loss_effect_digest IS NOT NULL AND
      decision_loss_effect_role='linked' AND
      decision_loss_effect_digest=decision_loss_effect_key AND
      decision_loss_after_id IS NOT NULL AND
      decision_loss_after_revision IS NOT NULL AND
      decision_loss_after_semantic_digest IS NOT NULL AND
      decision_loss_after_source_ref_digest IS NOT NULL)""",
        )
        db.rollback()

    def test_terminal_fresh_without_linked_loss_succeeds(self) -> None:
        db = database()
        seed_terminal_fresh_prepare(
            db, completion_has_loss=False, handoff_has_loss=False
        )
        db.execute("BEGIN")
        insert_fresh_commit(db, with_loss=False)
        insert_apply(
            db, batch_id="batch-fresh", apply_id="apply-fresh",
            apply_digest="apply-digest-fresh",
            transition_kind="custody-terminal",
            apply_kind="terminal-fresh", replay="replay-fresh",
            mutation="terminal-plan", handoff_has_loss=False,
        )
        db.commit()

    def test_terminal_fresh_with_exact_linked_loss_succeeds(self) -> None:
        db = database()
        seed_terminal_fresh_prepare(
            db, completion_has_loss=True, handoff_has_loss=True
        )
        db.execute("BEGIN")
        insert_loss_revision_and_head(db)
        insert_fresh_commit(db, with_loss=True)
        insert_apply(
            db, batch_id="batch-fresh", apply_id="apply-fresh",
            apply_digest="apply-digest-fresh",
            transition_kind="custody-terminal",
            apply_kind="terminal-fresh", replay="replay-fresh",
            mutation="terminal-plan", handoff_has_loss=True,
        )
        db.commit()

    def test_terminal_fresh_effect_only_is_rejected(self) -> None:
        db = database()
        seed_terminal_fresh_prepare(
            db, completion_has_loss=True, handoff_has_loss=False
        )
        db.execute("BEGIN")
        insert_fresh_commit(db, with_loss=False)
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            insert_apply(
                db, batch_id="batch-fresh", apply_id="apply-fresh",
                apply_digest="apply-digest-fresh",
                transition_kind="custody-terminal",
                apply_kind="terminal-fresh", replay="replay-fresh",
                mutation="terminal-plan", handoff_has_loss=False,
            )
        self.assertEqual(
            str(caught.exception),
            "lifecycle-terminal-fresh-linked-loss-crossed",
        )
        db.rollback()

    def test_terminal_fresh_apply_only_is_rejected(self) -> None:
        db = database()
        seed_terminal_fresh_prepare(
            db, completion_has_loss=False, handoff_has_loss=True
        )
        db.execute("BEGIN")
        insert_loss_revision_and_head(db)
        insert_fresh_commit(db, with_loss=True)
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            insert_apply(
                db, batch_id="batch-fresh", apply_id="apply-fresh",
                apply_digest="apply-digest-fresh",
                transition_kind="custody-terminal",
                apply_kind="terminal-fresh", replay="replay-fresh",
                mutation="terminal-plan", handoff_has_loss=True,
            )
        self.assertEqual(
            str(caught.exception),
            "lifecycle-terminal-fresh-linked-loss-crossed",
        )
        db.rollback()

    def test_fresh_commit_child_first_is_deferred_but_guarded(self) -> None:
        db = database()
        seed_terminal_fresh_prepare(
            db, completion_has_loss=False, handoff_has_loss=False
        )

        def commit_without_apply(connection: sqlite3.Connection) -> None:
            insert_fresh_commit(connection, with_loss=False)

        def commit_with_apply(connection: sqlite3.Connection) -> None:
            commit_without_apply(connection)
            insert_apply(
                connection,
                batch_id="batch-fresh",
                apply_id="apply-fresh",
                apply_digest="apply-digest-fresh",
                transition_kind="custody-terminal",
                apply_kind="terminal-fresh",
                replay="replay-fresh",
                mutation="terminal-plan",
            )

        assert_fk_rejected(
            db,
            invalid_operation=commit_without_apply,
            positive_control=commit_with_apply,
            expected=frozenset(
                {
                    ForeignKeySpec(
                        "lifecycle_fresh_rotation_commits",
                        (
                            "apply_id",
                            "fresh_apply_digest",
                            "generation_loss_after_key",
                        ),
                        "lifecycle_transition_applies",
                        (
                            "apply_id",
                            "apply_digest",
                            "fresh_generation_loss_after_key",
                        ),
                    )
                }
            ),
        )

    def test_other_materialized_children_are_child_first_and_guarded(self) -> None:
        cases = {
            "custody": (
                "lifecycle_rotation_custody_revisions",
                "INSERT INTO lifecycle_rotation_custody_revisions VALUES (?,?,?)",
            ),
            "loss": (
                "lifecycle_generation_loss_revisions",
                """INSERT INTO lifecycle_generation_loss_revisions VALUES
                (?,?,?,?,?,?,?,?,?,?,?)""",
            ),
            "retirement": (
                "agent_lifecycle_recovery_retirements",
                "INSERT INTO agent_lifecycle_recovery_retirements VALUES (?,?,?)",
            ),
        }
        for name, (_table, statement) in cases.items():
            with self.subTest(child=name, outcome="missing-parent"):
                db = database()
                db.execute("BEGIN")
                insert_batch(
                    db, batch_id=f"batch-{name}", apply_id=f"apply-{name}",
                    transition_kind={
                        "custody": "custody-terminal",
                        "loss": "generation-loss-terminal",
                        "retirement": "custody-recovery-retirement",
                    }[name],
                )
                db.commit()

                def insert_child(connection: sqlite3.Connection) -> None:
                    if name == "loss":
                        connection.execute(
                            statement,
                            (*SCOPE, "loss-child", 1, "sem-child", "src-child",
                             "journal-child", f"batch-{name}", f"apply-{name}",
                             f"digest-{name}"),
                        )
                    else:
                        connection.execute(
                            statement,
                            (f"{name}-child", f"apply-{name}", f"digest-{name}"),
                        )

                def insert_child_and_apply(connection: sqlite3.Connection) -> None:
                    insert_child(connection)
                    insert_apply(
                        connection,
                        batch_id=f"batch-{name}",
                        apply_id=f"apply-{name}",
                        apply_digest=f"digest-{name}",
                        transition_kind={
                            "custody": "custody-terminal",
                            "loss": "generation-loss-terminal",
                            "retirement": "custody-recovery-retirement",
                        }[name],
                    )

                assert_fk_rejected(
                    db,
                    invalid_operation=insert_child,
                    positive_control=insert_child_and_apply,
                    expected=frozenset(
                        {
                            ForeignKeySpec(
                                _table,
                                ("receipt_apply_id", "receipt_apply_digest"),
                                "lifecycle_transition_applies",
                                ("apply_id", "apply_digest"),
                            )
                        }
                    ),
                )


if __name__ == "__main__":
    unittest.main(verbosity=2)
