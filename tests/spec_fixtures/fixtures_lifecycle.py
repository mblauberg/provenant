#!/usr/bin/env python3
"""
Executable SQLite fixtures proving five substantiated spec defects are
accepted by DDL in the current semantic persistence, lifecycle-custody,
recovery and adjacent capability/route-lineage owners. Those current owners
contain DDL derived from the historical freeze at commit d7f3536; Git retains
the former specification path and its full history.

Every CREATE TABLE below transliterates the spec's bracket-list pseudo-DDL
into real SQLite syntax, verbatim in column set / PRIMARY KEY / UNIQUE / CHECK
/ FOREIGN KEY for the tables each lead is built around ("focus tables").
Referenced-but-unrelated parent tables are stubbed minimally: just the
columns and UNIQUE/PRIMARY KEY needed so SQLite's `PRAGMA foreign_keys=ON`
enforcement has a schema to check against (SQLite requires the parent table
to exist, and the exact referenced column tuple to be covered by a
PRIMARY KEY or UNIQUE index, for *any* insert into a table that declares the
FK -- even when the FK's own columns are NULL and the constraint is
therefore vacuously satisfied at that row).

Each lead runs in its own throwaway `:memory:` connection so the five
reproductions cannot interfere with one another. No timestamps or random
values are used anywhere; every id/digest is a fixed literal string, so the
script's output is byte-identical across runs.

Usage:
    python3 fixtures_lifecycle.py
Exit code 0 iff every defect reproduced as predicted (LEAD1/4/6/8 ACCEPTED,
LEAD3 CONFIRMED). A results summary is also written next to this script as
results_lifecycle.txt.
"""

import os
import sqlite3
import sys
import traceback

RESULTS_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                             "results_lifecycle.txt")


# ---------------------------------------------------------------------------
# Shared table DDL reused verbatim across leads (lifecycle_transition_applies
# and lifecycle_receipt_batches are each a "focus table" for more than one
# lead; kept as module constants so every use is byte-identical).
# ---------------------------------------------------------------------------

TRANSITION_APPLIES_SQL = """
CREATE TABLE lifecycle_transition_applies(
  apply_id PRIMARY KEY,
  apply_kind CHECK(apply_kind IN ('terminal','terminal-fresh','fresh')),
  receipt_batch_id UNIQUE, batch_completion_digest, transition_replay_digest,
  ordered_authority_receipt_set_digest, verified_scope_checkpoint_digest,
  applied_mutation_plan_digest,
  fresh_handoff_id UNIQUE, fresh_handoff_digest,
  fresh_project_session_id, fresh_run_id, fresh_agent_id, fresh_source_mode,
  fresh_apply_plan_digest, new_custody_id UNIQUE, new_custody_semantic_digest,
  new_custody_source_ref_digest, fresh_generation_loss_id,
  fresh_generation_loss_after_revision,
  fresh_generation_loss_after_semantic_digest,
  fresh_generation_loss_after_source_ref_digest, local_write_set_digest,
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
  UNIQUE(apply_id,apply_digest,fresh_project_session_id,fresh_run_id,
    fresh_agent_id,fresh_generation_loss_id,
    fresh_generation_loss_after_revision,
    fresh_generation_loss_after_semantic_digest,
    fresh_generation_loss_after_source_ref_digest),
  FOREIGN KEY(receipt_batch_id,apply_id,transition_replay_digest,
      applied_mutation_plan_digest)
    REFERENCES lifecycle_receipt_batches(
      batch_id,planned_apply_id,transition_replay_digest,mutation_plan_digest),
  FOREIGN KEY(receipt_batch_id,apply_id,transition_replay_digest,
      applied_mutation_plan_digest,fresh_handoff_id,fresh_handoff_digest)
    REFERENCES lifecycle_receipt_batches(
      batch_id,planned_apply_id,transition_replay_digest,mutation_plan_digest,
      fresh_handoff_id,fresh_handoff_digest),
  FOREIGN KEY(receipt_batch_id,batch_completion_digest,
      ordered_authority_receipt_set_digest,verified_scope_checkpoint_digest)
    REFERENCES lifecycle_receipt_batch_authorizations(
      batch_id,batch_completion_digest,ordered_authority_receipt_set_digest,
      verified_scope_checkpoint_digest),
  FOREIGN KEY(fresh_handoff_id,fresh_handoff_digest,apply_id,
      fresh_project_session_id,fresh_run_id,fresh_agent_id,fresh_source_mode,
      new_custody_id,
      new_custody_semantic_digest,new_custody_source_ref_digest,
      fresh_apply_plan_digest,fresh_generation_loss_id,
      fresh_generation_loss_after_revision,
      fresh_generation_loss_after_semantic_digest,
      fresh_generation_loss_after_source_ref_digest)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
      agent_id,source_mode,new_custody_id,new_custody_semantic_digest,
      new_custody_source_ref_digest,
      fresh_apply_plan_digest,affected_generation_loss_id,
      affected_generation_loss_after_revision,
      affected_generation_loss_after_semantic_digest,
      affected_generation_loss_after_source_ref_digest),
  CHECK((apply_kind='terminal' AND receipt_batch_id IS NOT NULL AND
      batch_completion_digest IS NOT NULL AND
      transition_replay_digest IS NOT NULL AND
      ordered_authority_receipt_set_digest IS NOT NULL AND
      verified_scope_checkpoint_digest IS NOT NULL AND
      applied_mutation_plan_digest IS NOT NULL AND fresh_handoff_id IS NULL AND
      fresh_handoff_digest IS NULL AND fresh_project_session_id IS NULL AND
      fresh_run_id IS NULL AND fresh_agent_id IS NULL AND
      fresh_source_mode IS NULL AND fresh_apply_plan_digest IS NULL AND
      new_custody_id IS NULL AND new_custody_semantic_digest IS NULL AND
      new_custody_source_ref_digest IS NULL AND
      fresh_generation_loss_id IS NULL AND
      fresh_generation_loss_after_revision IS NULL AND
      fresh_generation_loss_after_semantic_digest IS NULL AND
      fresh_generation_loss_after_source_ref_digest IS NULL) OR
    (apply_kind='terminal-fresh' AND receipt_batch_id IS NOT NULL AND
      batch_completion_digest IS NOT NULL AND
      transition_replay_digest IS NOT NULL AND
      ordered_authority_receipt_set_digest IS NOT NULL AND
      verified_scope_checkpoint_digest IS NOT NULL AND
      applied_mutation_plan_digest IS NOT NULL AND fresh_handoff_id IS NOT NULL AND
      fresh_handoff_digest IS NOT NULL AND fresh_project_session_id IS NOT NULL AND
      fresh_run_id IS NOT NULL AND fresh_agent_id IS NOT NULL AND
      fresh_source_mode='terminalize-nonfinal-custody' AND
      fresh_apply_plan_digest IS NOT NULL AND
      new_custody_id IS NOT NULL AND new_custody_semantic_digest IS NOT NULL AND
      new_custody_source_ref_digest IS NOT NULL AND
      ((fresh_generation_loss_id IS NULL AND
          fresh_generation_loss_after_revision IS NULL AND
          fresh_generation_loss_after_semantic_digest IS NULL AND
          fresh_generation_loss_after_source_ref_digest IS NULL) OR
        (fresh_generation_loss_id IS NOT NULL AND
          fresh_generation_loss_after_revision IS NOT NULL AND
          fresh_generation_loss_after_semantic_digest IS NOT NULL AND
          fresh_generation_loss_after_source_ref_digest IS NOT NULL))) OR
    (apply_kind='fresh' AND receipt_batch_id IS NULL AND
      batch_completion_digest IS NULL AND
      transition_replay_digest IS NULL AND
      ordered_authority_receipt_set_digest IS NULL AND
      verified_scope_checkpoint_digest IS NULL AND
      applied_mutation_plan_digest IS NOT NULL AND fresh_handoff_id IS NOT NULL AND
      fresh_handoff_digest IS NOT NULL AND fresh_project_session_id IS NOT NULL AND
      fresh_run_id IS NOT NULL AND fresh_agent_id IS NOT NULL AND
      fresh_source_mode IN ('reuse-final-custody','open-generation-loss') AND
      fresh_apply_plan_digest IS NOT NULL AND
      new_custody_id IS NOT NULL AND new_custody_semantic_digest IS NOT NULL AND
      new_custody_source_ref_digest IS NOT NULL AND
      applied_mutation_plan_digest=fresh_apply_plan_digest AND
      ((fresh_source_mode='reuse-final-custody' AND
          fresh_generation_loss_id IS NULL AND
          fresh_generation_loss_after_revision IS NULL AND
          fresh_generation_loss_after_semantic_digest IS NULL AND
          fresh_generation_loss_after_source_ref_digest IS NULL) OR
        (fresh_source_mode='open-generation-loss' AND
          fresh_generation_loss_id IS NOT NULL AND
          fresh_generation_loss_after_revision IS NOT NULL AND
          fresh_generation_loss_after_semantic_digest IS NOT NULL AND
          fresh_generation_loss_after_source_ref_digest IS NOT NULL))))
)
"""

RECEIPT_BATCHES_SQL = """
CREATE TABLE lifecycle_receipt_batches(
  batch_id PRIMARY KEY, planned_apply_id UNIQUE,
  project_session_id, run_id, agent_id,
  transition_kind CHECK(transition_kind IN
    ('custody-terminal','generation-loss-terminal',
      'custody-recovery-retirement')),
  effects_set_digest, mutation_plan_digest,
  transition_replay_json, transition_replay_digest,
  ordered_subject_set_digest,
  receipt_intent_count CHECK(receipt_intent_count IN (1,2)),
  review_adoption_reservation_id, review_adoption_reservation_digest,
  fresh_handoff_id, fresh_handoff_digest,
  recovery_retirement_id, recovery_retirement_plan_digest, created_at,
  UNIQUE(project_session_id,run_id,agent_id,transition_replay_digest),
  UNIQUE(batch_id,planned_apply_id),
  UNIQUE(batch_id,transition_kind,receipt_intent_count),
  UNIQUE(batch_id,planned_apply_id,transition_replay_digest,
    mutation_plan_digest),
  UNIQUE(batch_id,project_session_id,run_id),
  UNIQUE(batch_id,project_session_id,run_id,agent_id),
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id),
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
    review_adoption_reservation_digest),
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
    transition_kind),
  UNIQUE(batch_id,planned_apply_id,transition_replay_digest,
    mutation_plan_digest,fresh_handoff_id,fresh_handoff_digest),
  CHECK((review_adoption_reservation_id IS NULL)=
    (review_adoption_reservation_digest IS NULL)),
  CHECK((fresh_handoff_id IS NULL)=(fresh_handoff_digest IS NULL)),
  CHECK((recovery_retirement_id IS NULL)=
    (recovery_retirement_plan_digest IS NULL)),
  CHECK((transition_kind='custody-recovery-retirement')=
    (recovery_retirement_id IS NOT NULL)),
  CHECK((receipt_intent_count=2)=
    (review_adoption_reservation_id IS NOT NULL)),
  FOREIGN KEY(review_adoption_reservation_id,
      review_adoption_reservation_digest)
    REFERENCES lifecycle_review_adoption_reservations(
      reservation_id,reservation_digest),
  FOREIGN KEY(fresh_handoff_id,fresh_handoff_digest,planned_apply_id)
    REFERENCES lifecycle_fresh_recovery_handoffs(
      handoff_id,handoff_digest,planned_apply_id),
  FOREIGN KEY(recovery_retirement_id,recovery_retirement_plan_digest,
      planned_apply_id,project_session_id,run_id,agent_id,mutation_plan_digest)
    REFERENCES lifecycle_recovery_retirement_plans(
      retirement_id,retirement_plan_digest,planned_apply_id,
      project_session_id,run_id,agent_id,mutation_plan_digest)
)
"""

# Minimal stubs shared by LEAD4/LEAD6: a fresh-recovery-handoffs stub wide
# enough to satisfy both lifecycle_receipt_batches' 3-col FK and
# lifecycle_transition_applies' 15-col FK target (SQLite requires a UNIQUE
# index over the *exact* referenced column tuple, even when every value in
# that tuple is NULL for the row being inserted).
FRESH_HANDOFFS_STUB_SQL = """
CREATE TABLE lifecycle_fresh_recovery_handoffs(
  handoff_id, handoff_digest, planned_apply_id,
  project_session_id, run_id, agent_id, source_mode,
  new_custody_id, new_custody_semantic_digest, new_custody_source_ref_digest,
  fresh_apply_plan_digest, affected_generation_loss_id,
  affected_generation_loss_after_revision,
  affected_generation_loss_after_semantic_digest,
  affected_generation_loss_after_source_ref_digest,
  UNIQUE(handoff_id,handoff_digest,planned_apply_id),
  UNIQUE(handoff_id,handoff_digest,planned_apply_id,project_session_id,
    run_id,agent_id,source_mode,new_custody_id,new_custody_semantic_digest,
    new_custody_source_ref_digest,fresh_apply_plan_digest,
    affected_generation_loss_id,affected_generation_loss_after_revision,
    affected_generation_loss_after_semantic_digest,
    affected_generation_loss_after_source_ref_digest)
)
"""

RECEIPT_BATCH_AUTHORIZATIONS_STUB_SQL = """
CREATE TABLE lifecycle_receipt_batch_authorizations(
  batch_id, batch_completion_digest, ordered_authority_receipt_set_digest,
  verified_scope_checkpoint_digest,
  UNIQUE(batch_id,batch_completion_digest,ordered_authority_receipt_set_digest,
    verified_scope_checkpoint_digest)
)
"""

REVIEW_ADOPTION_RESERVATIONS_STUB_SQL = """
CREATE TABLE lifecycle_review_adoption_reservations(
  reservation_id, reservation_digest,
  UNIQUE(reservation_id,reservation_digest)
)
"""

RECOVERY_RETIREMENT_PLANS_STUB_SQL = """
CREATE TABLE lifecycle_recovery_retirement_plans(
  retirement_id, retirement_plan_digest, planned_apply_id,
  project_session_id, run_id, agent_id, mutation_plan_digest,
  UNIQUE(retirement_id,retirement_plan_digest,planned_apply_id,
    project_session_id,run_id,agent_id,mutation_plan_digest)
)
"""


def _connect():
    con = sqlite3.connect(":memory:")
    con.execute("PRAGMA foreign_keys=ON")
    return con


# ---------------------------------------------------------------------------
# LEAD1: fresh-apply leaves zero external receipts.
#
# Focus tables (full transcription): lifecycle_transition_applies
# (spec ~6500-6616), lifecycle_rotation_custody_revisions (~5954-6028),
# lifecycle_receipt_scope_checkpoints (~6056-6077), plus the namespace
# membership pair (~6096-6125) built for completeness.
# ---------------------------------------------------------------------------

def lead1():
    con = _connect()
    cur = con.cursor()

    cur.execute(TRANSITION_APPLIES_SQL)

    cur.execute("""
    CREATE TABLE lifecycle_rotation_custody_revisions(
      project_session_id, run_id, agent_id, custody_id,
      revision CHECK(revision >= 1), prior_revision, prior_journal_digest,
      state CHECK(state IN ('awaiting-boundary','prepared','dispatched','accepted',
        'ambiguous','provider-terminal','committing','finalized')),
      disposition_code CHECK(disposition_code IN
        ('none','adopted','no-effect','quarantined','superseded','abandoned')),
      proof_kind CHECK(proof_kind IN ('none','zero-dispatch-no-effect',
        'predispatch-superseded','postterminal-adoption-cas-superseded',
        'fresh-handoff-superseded','provider-terminal','provider-no-effect',
        'integrity-quarantine','confirmed-abandon')),
      terminal_evidence_digest,
      semantic_json, semantic_digest, source_ref_digest,
      origin_fresh_apply_id, origin_fresh_apply_digest,
      receipt_batch_id, receipt_apply_id, receipt_apply_digest,
      journal_json, journal_digest, recorded_at,
      PRIMARY KEY(run_id,agent_id,custody_id,revision),
      UNIQUE(project_session_id,run_id,agent_id,custody_id,revision),
      UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
        source_ref_digest),
      UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
        journal_digest),
      UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
        source_ref_digest,journal_digest),
      UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
        semantic_digest,source_ref_digest,journal_digest),
      UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
        semantic_digest,source_ref_digest,journal_digest,origin_fresh_apply_id,
        origin_fresh_apply_digest),
      UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,state,
        disposition_code,semantic_digest,source_ref_digest,journal_digest),
      UNIQUE(semantic_digest), UNIQUE(source_ref_digest), UNIQUE(journal_digest),
      CHECK((revision=1 AND prior_revision IS NULL AND
          prior_journal_digest IS NULL) OR
        (revision>1 AND prior_revision=revision-1 AND
          prior_journal_digest IS NOT NULL)),
      CHECK((state='finalized')=(disposition_code<>'none')),
      CHECK((state='finalized')=
        (receipt_batch_id IS NOT NULL AND receipt_apply_id IS NOT NULL AND
          receipt_apply_digest IS NOT NULL)),
      CHECK((receipt_batch_id IS NULL)=(receipt_apply_id IS NULL)),
      CHECK((receipt_batch_id IS NULL)=(receipt_apply_digest IS NULL)),
      CHECK((origin_fresh_apply_id IS NULL)=(origin_fresh_apply_digest IS NULL)),
      CHECK(origin_fresh_apply_id IS NULL OR
        (revision=1 AND state<>'finalized' AND receipt_batch_id IS NULL)),
      CHECK((state IN ('provider-terminal','committing','finalized'))=
        (terminal_evidence_digest IS NOT NULL)),
      CHECK((state='finalized')=(proof_kind<>'none')),
      FOREIGN KEY(project_session_id,run_id,agent_id,custody_id)
        REFERENCES lifecycle_rotation_custodies(
          project_session_id,run_id,agent_id,custody_id),
      FOREIGN KEY(project_session_id,run_id,agent_id,custody_id,prior_revision,
          prior_journal_digest)
        REFERENCES lifecycle_rotation_custody_revisions(
          project_session_id,run_id,agent_id,custody_id,revision,journal_digest),
      FOREIGN KEY(receipt_batch_id,receipt_apply_id,project_session_id,run_id,agent_id,
          custody_id,revision,semantic_digest,source_ref_digest)
        REFERENCES lifecycle_receipt_custody_effects(
          batch_id,planned_apply_id,project_session_id,run_id,agent_id,custody_id,
          final_revision,final_semantic_digest,final_source_ref_digest)
        DEFERRABLE INITIALLY DEFERRED,
      FOREIGN KEY(receipt_apply_id,receipt_apply_digest,receipt_batch_id)
        REFERENCES lifecycle_transition_applies(
          apply_id,apply_digest,receipt_batch_id)
        DEFERRABLE INITIALLY DEFERRED,
      FOREIGN KEY(receipt_apply_id,receipt_batch_id)
        REFERENCES lifecycle_transition_applies(apply_id,receipt_batch_id)
        DEFERRABLE INITIALLY DEFERRED,
      FOREIGN KEY(origin_fresh_apply_id,origin_fresh_apply_digest,custody_id,
          semantic_digest,source_ref_digest)
        REFERENCES lifecycle_transition_applies(
          apply_id,apply_digest,new_custody_id,new_custody_semantic_digest,
          new_custody_source_ref_digest)
        DEFERRABLE INITIALLY DEFERRED
    )
    """)

    cur.execute("""
    CREATE TABLE lifecycle_receipt_scope_checkpoints(
      project_session_id, run_id, authority_id,
      receipt_count CHECK(receipt_count >= 0),
      head_authority_sequence CHECK(head_authority_sequence >= 0),
      head_receipt_digest, ordered_record_set_digest,
      checkpoint_json, checkpoint_digest, attestation, verified_at,
      PRIMARY KEY(project_session_id,run_id,receipt_count),
      UNIQUE(checkpoint_digest),
      UNIQUE(project_session_id,run_id,checkpoint_digest),
      UNIQUE(project_session_id,run_id,receipt_count,checkpoint_digest,
        head_receipt_digest),
      UNIQUE(project_session_id,run_id,authority_id,receipt_count,
        checkpoint_digest,head_receipt_digest),
      UNIQUE(project_session_id,run_id,authority_id,receipt_count,
        head_authority_sequence,head_receipt_digest,ordered_record_set_digest,
        checkpoint_digest),
      CHECK(receipt_count=head_authority_sequence),
      CHECK((receipt_count=0)=(head_receipt_digest IS NULL)),
      FOREIGN KEY(project_session_id,run_id,authority_id)
        REFERENCES lifecycle_admitted_run_scopes(
          project_session_id,run_id,authority_id)
    )
    """)

    # minimal namespace membership pair (~6096-6125), built for completeness;
    # not populated -- the "zero external receipts" proof does not need
    # cross-run namespace aggregation, only the per-scope checkpoint below.
    cur.execute("""
    CREATE TABLE lifecycle_receipt_namespace_checkpoints(
      project_id, authority_id, scope_count CHECK(scope_count >= 0),
      ordered_scope_head_set_digest, checkpoint_json, checkpoint_digest,
      attestation, verified_at,
      PRIMARY KEY(project_id,checkpoint_digest),
      UNIQUE(checkpoint_digest),
      UNIQUE(project_id,checkpoint_digest,authority_id),
      UNIQUE(project_id,authority_id,scope_count,ordered_scope_head_set_digest,
        checkpoint_digest)
    )
    """)
    cur.execute("""
    CREATE TABLE lifecycle_receipt_namespace_members(
      project_id, checkpoint_digest, ordinal CHECK(ordinal >= 1),
      project_session_id, run_id, authority_id, scope_checkpoint_digest, receipt_count,
      head_receipt_digest,
      PRIMARY KEY(project_id,checkpoint_digest,ordinal),
      UNIQUE(project_id,checkpoint_digest,project_session_id,run_id),
      CHECK(receipt_count >= 1 AND head_receipt_digest IS NOT NULL),
      FOREIGN KEY(project_id,checkpoint_digest,authority_id)
        REFERENCES lifecycle_receipt_namespace_checkpoints(
          project_id,checkpoint_digest,authority_id),
      FOREIGN KEY(project_id,project_session_id,run_id)
        REFERENCES lifecycle_admitted_run_scopes(
          project_id,project_session_id,run_id),
      FOREIGN KEY(project_session_id,run_id,authority_id,receipt_count,
          scope_checkpoint_digest,head_receipt_digest)
        REFERENCES lifecycle_receipt_scope_checkpoints(
          project_session_id,run_id,authority_id,receipt_count,checkpoint_digest,
          head_receipt_digest)
    )
    """)

    # minimal stub parents
    cur.execute("""
    CREATE TABLE lifecycle_rotation_custodies(
      project_session_id, run_id, agent_id, custody_id,
      PRIMARY KEY(project_session_id,run_id,agent_id,custody_id)
    )
    """)
    cur.execute("""
    CREATE TABLE lifecycle_receipt_custody_effects(
      batch_id, planned_apply_id, project_session_id, run_id, agent_id, custody_id,
      final_revision, final_semantic_digest, final_source_ref_digest,
      UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,custody_id,
        final_revision,final_semantic_digest,final_source_ref_digest)
    )
    """)
    cur.execute("""
    CREATE TABLE lifecycle_receipt_batches(
      batch_id, planned_apply_id, transition_replay_digest, mutation_plan_digest,
      fresh_handoff_id, fresh_handoff_digest, agent_id, run_id, project_session_id,
      UNIQUE(batch_id,planned_apply_id,transition_replay_digest,mutation_plan_digest),
      UNIQUE(batch_id,planned_apply_id,transition_replay_digest,mutation_plan_digest,
        fresh_handoff_id,fresh_handoff_digest)
    )
    """)
    cur.execute(RECEIPT_BATCH_AUTHORIZATIONS_STUB_SQL)
    cur.execute(FRESH_HANDOFFS_STUB_SQL)
    cur.execute("""
    CREATE TABLE lifecycle_admitted_run_scopes(
      project_id, project_session_id, run_id, authority_id,
      PRIMARY KEY(project_session_id,run_id),
      UNIQUE(project_id,project_session_id,run_id),
      UNIQUE(project_session_id,run_id,authority_id)
    )
    """)
    # side table solely for the "zero external receipts" assertion query
    cur.execute("""
    CREATE TABLE lifecycle_authority_receipts(
      intent_digest PRIMARY KEY, agent_id, run_id, project_session_id
    )
    """)

    con.commit()

    # ---------------- bad-state insert sequence ----------------
    RUN = "run-1"; AGENT = "agent-1"; PSID = "ps-1"; AUTH = "auth-1"
    CUSTODY = "custody-1"
    APPLY_ID = "apply-fresh-1"; APPLY_DIGEST = "digest-apply-fresh-1"
    HANDOFF_ID = "handoff-1"; HANDOFF_DIGEST = "digest-handoff-1"
    GLOSS_ID = "gloss-1"
    NEW_SEM = "sem-custody-1"; NEW_SRC = "src-custody-1"
    APPLY_PLAN_DIGEST = "plan-digest-1"
    GLOSS_AFTER_SEM = "sem-gloss-after-1"; GLOSS_AFTER_SRC = "src-gloss-after-1"

    cur.execute("INSERT INTO lifecycle_rotation_custodies VALUES (?,?,?,?)",
                (PSID, RUN, AGENT, CUSTODY))
    cur.execute("INSERT INTO lifecycle_admitted_run_scopes VALUES (?,?,?,?)",
                ("proj-1", PSID, RUN, AUTH))

    # minimal handoff stub row backing the fresh apply's mandatory FK
    cur.execute("""
    INSERT INTO lifecycle_fresh_recovery_handoffs
      (handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,
       agent_id,source_mode,new_custody_id,new_custody_semantic_digest,
       new_custody_source_ref_digest,fresh_apply_plan_digest,
       affected_generation_loss_id,affected_generation_loss_after_revision,
       affected_generation_loss_after_semantic_digest,
       affected_generation_loss_after_source_ref_digest)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """, (HANDOFF_ID, HANDOFF_DIGEST, APPLY_ID, PSID, RUN, AGENT,
          "open-generation-loss", CUSTODY, NEW_SEM, NEW_SRC, APPLY_PLAN_DIGEST,
          GLOSS_ID, 2, GLOSS_AFTER_SEM, GLOSS_AFTER_SRC))

    # the fresh apply row itself -- apply_kind='fresh', fresh_source_mode='open-generation-loss'
    cur.execute("""
    INSERT INTO lifecycle_transition_applies
      (apply_id, apply_kind, receipt_batch_id, batch_completion_digest,
       transition_replay_digest, ordered_authority_receipt_set_digest,
       verified_scope_checkpoint_digest, applied_mutation_plan_digest,
       fresh_handoff_id, fresh_handoff_digest, fresh_project_session_id,
       fresh_run_id, fresh_agent_id, fresh_source_mode, fresh_apply_plan_digest,
       new_custody_id, new_custody_semantic_digest, new_custody_source_ref_digest,
       fresh_generation_loss_id, fresh_generation_loss_after_revision,
       fresh_generation_loss_after_semantic_digest,
       fresh_generation_loss_after_source_ref_digest,
       local_write_set_digest, apply_json, apply_digest, applied_at)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """, (APPLY_ID, "fresh", None, None, None, None, None, APPLY_PLAN_DIGEST,
          HANDOFF_ID, HANDOFF_DIGEST, PSID, RUN, AGENT, "open-generation-loss",
          APPLY_PLAN_DIGEST, CUSTODY, NEW_SEM, NEW_SRC, GLOSS_ID, 2,
          GLOSS_AFTER_SEM, GLOSS_AFTER_SRC, "lw-1", "{}", APPLY_DIGEST, "t0"))

    # the custody revision-1 row created by the fresh apply: receipt_batch_id
    # NULL, no receipt effects, no authority receipts anywhere.
    cur.execute("""
    INSERT INTO lifecycle_rotation_custody_revisions
      (project_session_id, run_id, agent_id, custody_id, revision, prior_revision,
       prior_journal_digest, state, disposition_code, proof_kind,
       terminal_evidence_digest, semantic_json, semantic_digest, source_ref_digest,
       origin_fresh_apply_id, origin_fresh_apply_digest, receipt_batch_id,
       receipt_apply_id, receipt_apply_digest, journal_json, journal_digest,
       recorded_at)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """, (PSID, RUN, AGENT, CUSTODY, 1, None, None, "accepted", "none", "none",
          None, "{}", NEW_SEM, NEW_SRC, APPLY_ID, APPLY_DIGEST, None, None, None,
          "{}", "journal-1", "t0"))

    # a scope checkpoint that proves the authority's receipt count is zero
    cur.execute("""
    INSERT INTO lifecycle_receipt_scope_checkpoints
      (project_session_id, run_id, authority_id, receipt_count,
       head_authority_sequence, head_receipt_digest, ordered_record_set_digest,
       checkpoint_json, checkpoint_digest, attestation, verified_at)
    VALUES (?,?,?,?,?,?,?,?,?,?,?)
    """, (PSID, RUN, AUTH, 0, 0, None, "empty-set-digest", "{}",
          "checkpoint-digest-zero", "attest-1", "t0"))

    con.commit()

    # ---------------- assertions ----------------
    n_receipts = cur.execute(
        "SELECT COUNT(*) FROM lifecycle_authority_receipts WHERE agent_id=?",
        (AGENT,)).fetchone()[0]
    n_batches = cur.execute(
        "SELECT COUNT(*) FROM lifecycle_receipt_batches WHERE agent_id=?",
        (AGENT,)).fetchone()[0]
    apply_row = cur.execute(
        "SELECT apply_kind FROM lifecycle_transition_applies WHERE apply_id=?",
        (APPLY_ID,)).fetchone()
    custody_row = cur.execute(
        "SELECT receipt_batch_id FROM lifecycle_rotation_custody_revisions "
        "WHERE run_id=? AND agent_id=? AND custody_id=? AND revision=1",
        (RUN, AGENT, CUSTODY)).fetchone()
    checkpoint_row = cur.execute(
        "SELECT receipt_count FROM lifecycle_receipt_scope_checkpoints "
        "WHERE project_session_id=? AND run_id=? AND authority_id=?",
        (PSID, RUN, AUTH)).fetchone()

    assert n_receipts == 0, "expected zero lifecycle_authority_receipts rows"
    assert n_batches == 0, "expected zero lifecycle_receipt_batches rows"
    assert apply_row is not None and apply_row[0] == "fresh"
    assert custody_row is not None and custody_row[0] is None
    assert checkpoint_row is not None and checkpoint_row[0] == 0

    con.close()
    return "ACCEPTED", (
        "fresh apply (apply_kind='fresh', fresh_source_mode='open-generation-loss') "
        "committed custody revision 1 with receipt_batch_id NULL; "
        "lifecycle_authority_receipts and lifecycle_receipt_batches remain empty "
        "for the agent and the scope checkpoint shows receipt_count=0"
    )


# ---------------------------------------------------------------------------
# LEAD3: reservation pre-authority FK to a not-yet-materialized revision.
#
# Focus tables (full transcription): lifecycle_review_adoption_reservations
# (~6618-6706), lifecycle_generation_loss_revisions (~7094-7199).
# ---------------------------------------------------------------------------

def lead3():
    con = _connect()
    cur = con.cursor()

    cur.execute("""
    CREATE TABLE lifecycle_review_adoption_reservations(
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
      decision_loss_after_key,
      recovery_source_decision_json, recovery_source_decision_digest,
      local_write_set_digest, reservation_json, created_at,
      UNIQUE(reservation_id,reservation_digest),
      UNIQUE(reservation_digest,project_session_id,run_id,agent_id,custody_id,
        finalized_custody_revision,review_decision_digest,certification_cut_key,
        decision_loss_after_key),
      UNIQUE(reservation_digest,decision_loss_after_id,decision_loss_after_revision,
        decision_loss_after_semantic_digest,decision_loss_after_source_ref_digest),
      UNIQUE(project_session_id,run_id,agent_id,custody_id,
        finalized_custody_revision),
      CHECK(certification_cut_key IS NOT NULL AND
        decision_loss_after_key IS NOT NULL),
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
      FOREIGN KEY(project_session_id,run_id,agent_id,decision_loss_after_id,
          decision_loss_after_revision,decision_loss_after_semantic_digest,
          decision_loss_after_source_ref_digest)
        REFERENCES lifecycle_generation_loss_revisions(
          project_session_id,run_id,agent_id,generation_loss_id,revision,
          semantic_digest,source_ref_digest)
    )
    """)

    cur.execute("""
    CREATE TABLE lifecycle_generation_loss_revisions(
      project_session_id, run_id, agent_id, generation_loss_id,
      revision CHECK(revision >= 1), prior_revision, prior_journal_digest,
      state CHECK(state IN
        ('open','recovery-in-progress','recovered-adopted','abandoned')),
      abandon_kind_code CHECK(
        abandon_kind_code IN ('none','direct-open','recovery-attempt')),
      recovery_action_adapter_id, recovery_action_id, active_recovery_custody_id,
      terminal_evidence_digest, semantic_json, semantic_digest, source_ref_digest,
      origin_fresh_apply_id, origin_fresh_apply_digest,
      receipt_batch_id, receipt_apply_id, receipt_apply_digest,
      journal_json, journal_digest, recorded_at,
      PRIMARY KEY(run_id,agent_id,generation_loss_id,revision),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
        source_ref_digest),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
        journal_digest),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
        source_ref_digest,journal_digest),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
        semantic_digest,source_ref_digest,journal_digest),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
        semantic_digest,source_ref_digest),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,state,
        source_ref_digest,journal_digest),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,state,
        active_recovery_custody_id,source_ref_digest,journal_digest),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
        semantic_digest,source_ref_digest,journal_digest,origin_fresh_apply_id,
        origin_fresh_apply_digest),
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,state,
        abandon_kind_code,recovery_action_adapter_id,recovery_action_id,
        active_recovery_custody_id,semantic_digest,source_ref_digest,journal_digest),
      UNIQUE(semantic_digest), UNIQUE(source_ref_digest), UNIQUE(journal_digest),
      CHECK((revision=1 AND prior_revision IS NULL AND
          prior_journal_digest IS NULL) OR
        (revision>1 AND prior_revision=revision-1 AND
          prior_journal_digest IS NOT NULL)),
      CHECK((receipt_batch_id IS NULL)=(receipt_apply_id IS NULL)),
      CHECK((receipt_batch_id IS NULL)=(receipt_apply_digest IS NULL)),
      CHECK((origin_fresh_apply_id IS NULL)=(origin_fresh_apply_digest IS NULL)),
      CHECK((revision=1 AND state='open' AND receipt_batch_id IS NULL AND
          origin_fresh_apply_id IS NULL) OR
        (revision>1 AND
          ((receipt_batch_id IS NOT NULL AND origin_fresh_apply_id IS NULL) OR
            (receipt_batch_id IS NULL AND origin_fresh_apply_id IS NOT NULL)))),
      CHECK(origin_fresh_apply_id IS NULL OR state='recovery-in-progress'),
      CHECK(state NOT IN ('recovered-adopted','abandoned') OR
        receipt_batch_id IS NOT NULL),
      CHECK((state='open' AND abandon_kind_code='none' AND
          recovery_action_id IS NULL AND active_recovery_custody_id IS NULL AND
          terminal_evidence_digest IS NULL) OR
        (state='recovery-in-progress' AND abandon_kind_code='none' AND
          recovery_action_id IS NOT NULL AND
          active_recovery_custody_id IS NOT NULL AND
          terminal_evidence_digest IS NULL) OR
        (state='recovered-adopted' AND abandon_kind_code='none' AND
          recovery_action_id IS NOT NULL AND
          active_recovery_custody_id IS NOT NULL AND
          terminal_evidence_digest IS NOT NULL) OR
        (state='abandoned' AND abandon_kind_code='direct-open' AND
          recovery_action_id IS NULL AND active_recovery_custody_id IS NULL AND
          terminal_evidence_digest IS NOT NULL) OR
        (state='abandoned' AND abandon_kind_code='recovery-attempt' AND
          recovery_action_id IS NOT NULL AND
          active_recovery_custody_id IS NOT NULL AND
          terminal_evidence_digest IS NOT NULL)),
      CHECK((recovery_action_adapter_id IS NULL)=(recovery_action_id IS NULL)),
      FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id)
        REFERENCES lifecycle_generation_losses(
          project_session_id,run_id,agent_id,generation_loss_id),
      FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
          prior_revision,prior_journal_digest)
        REFERENCES lifecycle_generation_loss_revisions(
          project_session_id,run_id,agent_id,generation_loss_id,revision,
          journal_digest),
      FOREIGN KEY(recovery_action_adapter_id,recovery_action_id)
        REFERENCES lifecycle_rotation_custodies(
          provider_action_adapter_id,provider_action_id),
      FOREIGN KEY(run_id,agent_id,active_recovery_custody_id)
        REFERENCES lifecycle_rotation_custodies(run_id,agent_id,custody_id),
      FOREIGN KEY(receipt_batch_id,receipt_apply_id,project_session_id,run_id,agent_id,
          generation_loss_id,revision,semantic_digest,source_ref_digest)
        REFERENCES lifecycle_receipt_generation_loss_effects(
          batch_id,planned_apply_id,project_session_id,run_id,agent_id,
          generation_loss_id,final_revision,final_semantic_digest,
          final_source_ref_digest)
        DEFERRABLE INITIALLY DEFERRED,
      FOREIGN KEY(receipt_apply_id,receipt_apply_digest,receipt_batch_id)
        REFERENCES lifecycle_transition_applies(
          apply_id,apply_digest,receipt_batch_id)
        DEFERRABLE INITIALLY DEFERRED,
      FOREIGN KEY(receipt_apply_id,receipt_batch_id)
        REFERENCES lifecycle_transition_applies(apply_id,receipt_batch_id)
        DEFERRABLE INITIALLY DEFERRED,
      FOREIGN KEY(origin_fresh_apply_id,origin_fresh_apply_digest,
          project_session_id,run_id,agent_id,generation_loss_id,revision,
          semantic_digest,source_ref_digest)
        REFERENCES lifecycle_transition_applies(
          apply_id,apply_digest,fresh_project_session_id,fresh_run_id,fresh_agent_id,
          fresh_generation_loss_id,fresh_generation_loss_after_revision,
          fresh_generation_loss_after_semantic_digest,
          fresh_generation_loss_after_source_ref_digest)
        DEFERRABLE INITIALLY DEFERRED
    )
    """)

    # minimal stub parents
    cur.execute("""
    CREATE TABLE lifecycle_generation_losses(
      project_session_id, run_id, agent_id, generation_loss_id,
      PRIMARY KEY(project_session_id,run_id,agent_id,generation_loss_id)
    )
    """)
    cur.execute("""
    CREATE TABLE lifecycle_rotation_custodies(
      project_session_id, run_id, agent_id, custody_id,
      provider_action_adapter_id, provider_action_id,
      PRIMARY KEY(project_session_id,run_id,agent_id,custody_id),
      UNIQUE(run_id,agent_id,custody_id),
      UNIQUE(provider_action_adapter_id,provider_action_id)
    )
    """)
    cur.execute("""
    CREATE TABLE lifecycle_rotation_custody_revisions(
      project_session_id, run_id, agent_id, custody_id, revision, source_ref_digest,
      UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,source_ref_digest)
    )
    """)
    cur.execute("""
    CREATE TABLE lifecycle_receipt_generation_loss_effects(
      batch_id, planned_apply_id, project_session_id, run_id, agent_id,
      generation_loss_id, final_revision, final_semantic_digest, final_source_ref_digest,
      UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
        generation_loss_id,final_revision,final_semantic_digest,final_source_ref_digest)
    )
    """)
    cur.execute("""
    CREATE TABLE lifecycle_transition_applies(
      apply_id, apply_digest, receipt_batch_id,
      fresh_project_session_id, fresh_run_id, fresh_agent_id,
      fresh_generation_loss_id, fresh_generation_loss_after_revision,
      fresh_generation_loss_after_semantic_digest,
      fresh_generation_loss_after_source_ref_digest,
      UNIQUE(apply_id,apply_digest,receipt_batch_id),
      UNIQUE(apply_id,receipt_batch_id),
      UNIQUE(apply_id,apply_digest,fresh_project_session_id,fresh_run_id,
        fresh_agent_id,fresh_generation_loss_id,
        fresh_generation_loss_after_revision,
        fresh_generation_loss_after_semantic_digest,
        fresh_generation_loss_after_source_ref_digest)
    )
    """)

    con.commit()

    # ---------------- bad-state insert sequence ----------------
    RUN = "run-3"; AGENT = "agent-3"; PSID = "ps-3"
    GLOSS = "gloss-3"; CUSTODY = "custody-3"

    cur.execute("INSERT INTO lifecycle_generation_losses VALUES (?,?,?,?)",
                (PSID, RUN, AGENT, GLOSS))
    cur.execute("INSERT INTO lifecycle_rotation_custodies VALUES (?,?,?,?,?,?)",
                (PSID, RUN, AGENT, CUSTODY, None, None))

    # the source generation-loss revision (revision 1, state='open') -- this
    # DOES exist at prepare time.
    cur.execute("""
    INSERT INTO lifecycle_generation_loss_revisions
      (project_session_id, run_id, agent_id, generation_loss_id, revision,
       prior_revision, prior_journal_digest, state, abandon_kind_code,
       recovery_action_adapter_id, recovery_action_id, active_recovery_custody_id,
       terminal_evidence_digest, semantic_json, semantic_digest, source_ref_digest,
       origin_fresh_apply_id, origin_fresh_apply_digest, receipt_batch_id,
       receipt_apply_id, receipt_apply_digest, journal_json, journal_digest,
       recorded_at)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """, (PSID, RUN, AGENT, GLOSS, 1, None, None, "open", "none", None, None,
          None, None, "{}", "sem-gl-1", "src-gl-1", None, None, None, None, None,
          "{}", "journal-gl-1", "t0"))

    con.commit()

    # The mandated prepare-time reservation insert: decision_loss_after_revision
    # points at the *future* recovered-adopted revision (2), which per the
    # revision>1 => receipt_batch_id NOT NULL CHECK can only be materialized
    # *after* the very apply/commit this reservation is supposed to gate.
    # That row does not exist yet.
    insert_sql = """
    INSERT INTO lifecycle_review_adoption_reservations
      (reservation_id, reservation_digest, project_session_id, run_id, agent_id,
       custody_id, finalized_custody_revision, target_generation,
       predecessor_binding_generation, predecessor_binding_digest,
       terminal_sequence_high_water, lifecycle_adoption_evidence_digest,
       review_decision_json, review_decision_digest, certification_cut_json,
       certification_cut_digest, certification_cut_key, recovery_source_kind,
       recovery_from_custody_id, recovery_from_custody_revision,
       recovery_from_generation_loss_id, recovery_from_generation_loss_revision,
       recovery_source_ref_digest, decision_loss_after_id,
       decision_loss_after_revision, decision_loss_after_semantic_digest,
       decision_loss_after_source_ref_digest, decision_loss_after_key,
       recovery_source_decision_json, recovery_source_decision_digest,
       local_write_set_digest, reservation_json, created_at)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """
    params = (
        "resv-3", "resv-digest-3", PSID, RUN, AGENT, CUSTODY, 1, 1,
        None, None, 0, "evidence-1",
        "{}", "rd-digest-1", None, None, "none", "generation-loss",
        None, None,
        GLOSS, 1,
        "src-gl-1",
        GLOSS, 2, "sem-gl-2", "src-gl-2", "src-gl-2",
        "{}", "rsd-digest-1",
        "lw-3", "{}", "t0",
    )

    try:
        cur.execute(insert_sql, params)
        con.commit()
        con.close()
        return "REJECTED", (
            "prepare-time reservation insert unexpectedly succeeded -- the "
            "ordering defect was NOT confirmed against this DDL"
        )
    except sqlite3.IntegrityError as e:
        if str(e) != "FOREIGN KEY constraint failed":
            con.close()
            return "REJECTED", (
                "prepare-time reservation insert hit the wrong constraint: "
                f"{type(e).__name__}: {e}"
            )
        con.close()
        return "CONFIRMED", (
            "inserting the generation-loss reservation with "
            "decision_loss_after_revision=2 (the not-yet-materialized "
            "recovered-adopted revision) was blocked: "
            f"{type(e).__name__}: {e}"
        )
    except sqlite3.OperationalError as e:
        con.close()
        return "REJECTED", (
            "prepare-time reservation insert hit an operational error instead "
            f"of the required foreign key: {e}"
        )


# ---------------------------------------------------------------------------
# LEAD4: terminal-fresh batch applied as plain terminal via null-vacuous FK.
#
# Focus tables: lifecycle_transition_applies (~6500-6616),
# lifecycle_receipt_batches (~6162-6210); lifecycle_fresh_recovery_handoffs
# (~6768-6825) stubbed minimally.
# ---------------------------------------------------------------------------

def lead4():
    con = _connect()
    cur = con.cursor()

    cur.execute(TRANSITION_APPLIES_SQL)
    cur.execute(RECEIPT_BATCHES_SQL)
    cur.execute(FRESH_HANDOFFS_STUB_SQL)
    cur.execute(RECEIPT_BATCH_AUTHORIZATIONS_STUB_SQL)
    cur.execute(REVIEW_ADOPTION_RESERVATIONS_STUB_SQL)
    cur.execute(RECOVERY_RETIREMENT_PLANS_STUB_SQL)
    con.commit()

    RUN = "run-4"; AGENT = "agent-4"; PSID = "ps-4"
    BATCH = "batch-4"; APPLY = "apply-4"
    HANDOFF_ID = "handoff-4"; HANDOFF_DIGEST = "handoffdig-4"
    REPLAY = "replay-4"; MUTPLAN = "mutplan-4"

    # the handoff row that made this a terminal-fresh operation in the plan
    cur.execute("""
    INSERT INTO lifecycle_fresh_recovery_handoffs
      (handoff_id, handoff_digest, planned_apply_id)
    VALUES (?,?,?)
    """, (HANDOFF_ID, HANDOFF_DIGEST, APPLY))

    # a batch that was planned as terminal-fresh (carries a non-null
    # fresh_handoff_id)
    cur.execute("""
    INSERT INTO lifecycle_receipt_batches
      (batch_id, planned_apply_id, project_session_id, run_id, agent_id,
       transition_kind, effects_set_digest, mutation_plan_digest,
       transition_replay_json, transition_replay_digest, ordered_subject_set_digest,
       receipt_intent_count, review_adoption_reservation_id,
       review_adoption_reservation_digest, fresh_handoff_id, fresh_handoff_digest,
       recovery_retirement_id, recovery_retirement_plan_digest, created_at)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """, (BATCH, APPLY, PSID, RUN, AGENT, "custody-terminal", "eff-4", MUTPLAN,
          "{}", REPLAY, "subj-4", 1, None, None, HANDOFF_ID, HANDOFF_DIGEST,
          None, None, "t0"))

    cur.execute("""
    INSERT INTO lifecycle_receipt_batch_authorizations VALUES (?,?,?,?)
    """, (BATCH, "completion-4", "authset-4", "checkpoint-4"))

    # the apply row matched to that batch by the 4-col FK: apply_kind='terminal'
    # with ALL fresh_* columns NULL (including fresh_handoff_id) even though
    # the batch it is completing carries fresh_handoff_id='handoff-4'.
    cur.execute("""
    INSERT INTO lifecycle_transition_applies
      (apply_id, apply_kind, receipt_batch_id, batch_completion_digest,
       transition_replay_digest, ordered_authority_receipt_set_digest,
       verified_scope_checkpoint_digest, applied_mutation_plan_digest,
       fresh_handoff_id, fresh_handoff_digest, fresh_project_session_id,
       fresh_run_id, fresh_agent_id, fresh_source_mode, fresh_apply_plan_digest,
       new_custody_id, new_custody_semantic_digest, new_custody_source_ref_digest,
       fresh_generation_loss_id, fresh_generation_loss_after_revision,
       fresh_generation_loss_after_semantic_digest,
       fresh_generation_loss_after_source_ref_digest,
       local_write_set_digest, apply_json, apply_digest, applied_at)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """, (APPLY, "terminal", BATCH, "completion-4", REPLAY, "authset-4",
          "checkpoint-4", MUTPLAN, None, None, None, None, None, None, None,
          None, None, None, None, None, None, None, "lw-4", "{}",
          "digest-apply-4", "t0"))

    con.commit()

    apply_row = cur.execute(
        "SELECT apply_kind, fresh_handoff_id, new_custody_id "
        "FROM lifecycle_transition_applies WHERE apply_id=?", (APPLY,)
    ).fetchone()
    batch_row = cur.execute(
        "SELECT fresh_handoff_id FROM lifecycle_receipt_batches WHERE batch_id=?",
        (BATCH,)
    ).fetchone()

    assert apply_row is not None
    assert apply_row[0] == "terminal"
    assert apply_row[1] is None
    assert apply_row[2] is None
    assert batch_row is not None and batch_row[0] == HANDOFF_ID

    con.close()
    return "ACCEPTED", (
        "apply_kind='terminal' apply row with all fresh_* columns NULL was "
        "accepted against a batch carrying fresh_handoff_id='handoff-4': the "
        "6-col batch FK is null-vacuous on the apply side and never fires, so "
        "the terminal-fresh batch finalizes as plain terminal with no new custody"
    )


# ---------------------------------------------------------------------------
# LEAD6: a bare apply row counts as applied (no post-state custody required).
#
# Focus table: lifecycle_transition_applies (~6500-6616) with its
# batch/authorization/handoff parents stubbed minimally.
# ---------------------------------------------------------------------------

CUSTODY_REVISIONS_FOR_LEAD6_SQL = """
CREATE TABLE lifecycle_rotation_custody_revisions(
  project_session_id, run_id, agent_id, custody_id,
  revision CHECK(revision >= 1), prior_revision, prior_journal_digest,
  state CHECK(state IN ('awaiting-boundary','prepared','dispatched','accepted',
    'ambiguous','provider-terminal','committing','finalized')),
  disposition_code CHECK(disposition_code IN
    ('none','adopted','no-effect','quarantined','superseded','abandoned')),
  proof_kind CHECK(proof_kind IN ('none','zero-dispatch-no-effect',
    'predispatch-superseded','postterminal-adoption-cas-superseded',
    'fresh-handoff-superseded','provider-terminal','provider-no-effect',
    'integrity-quarantine','confirmed-abandon')),
  terminal_evidence_digest,
  semantic_json, semantic_digest, source_ref_digest,
  origin_fresh_apply_id, origin_fresh_apply_digest,
  receipt_batch_id, receipt_apply_id, receipt_apply_digest,
  journal_json, journal_digest, recorded_at,
  PRIMARY KEY(run_id,agent_id,custody_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
    source_ref_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
    journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
    source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
    semantic_digest,source_ref_digest,journal_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
    semantic_digest,source_ref_digest,journal_digest,origin_fresh_apply_id,
    origin_fresh_apply_digest),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,state,
    disposition_code,semantic_digest,source_ref_digest,journal_digest),
  UNIQUE(semantic_digest), UNIQUE(source_ref_digest), UNIQUE(journal_digest),
  CHECK((revision=1 AND prior_revision IS NULL AND
      prior_journal_digest IS NULL) OR
    (revision>1 AND prior_revision=revision-1 AND
      prior_journal_digest IS NOT NULL)),
  CHECK((state='finalized')=(disposition_code<>'none')),
  CHECK((state='finalized')=
    (receipt_batch_id IS NOT NULL AND receipt_apply_id IS NOT NULL AND
      receipt_apply_digest IS NOT NULL)),
  CHECK((receipt_batch_id IS NULL)=(receipt_apply_id IS NULL)),
  CHECK((receipt_batch_id IS NULL)=(receipt_apply_digest IS NULL)),
  CHECK((origin_fresh_apply_id IS NULL)=(origin_fresh_apply_digest IS NULL)),
  CHECK(origin_fresh_apply_id IS NULL OR
    (revision=1 AND state<>'finalized' AND receipt_batch_id IS NULL)),
  CHECK((state IN ('provider-terminal','committing','finalized'))=
    (terminal_evidence_digest IS NOT NULL)),
  CHECK((state='finalized')=(proof_kind<>'none')),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id)
    REFERENCES lifecycle_rotation_custodies(
      project_session_id,run_id,agent_id,custody_id),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id,prior_revision,
      prior_journal_digest)
    REFERENCES lifecycle_rotation_custody_revisions(
      project_session_id,run_id,agent_id,custody_id,revision,journal_digest),
  FOREIGN KEY(receipt_batch_id,receipt_apply_id,project_session_id,run_id,agent_id,
      custody_id,revision,semantic_digest,source_ref_digest)
    REFERENCES lifecycle_receipt_custody_effects(
      batch_id,planned_apply_id,project_session_id,run_id,agent_id,custody_id,
      final_revision,final_semantic_digest,final_source_ref_digest)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest,receipt_batch_id)
    REFERENCES lifecycle_transition_applies(
      apply_id,apply_digest,receipt_batch_id)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(receipt_apply_id,receipt_batch_id)
    REFERENCES lifecycle_transition_applies(apply_id,receipt_batch_id)
    DEFERRABLE INITIALLY DEFERRED,
  FOREIGN KEY(origin_fresh_apply_id,origin_fresh_apply_digest,custody_id,
      semantic_digest,source_ref_digest)
    REFERENCES lifecycle_transition_applies(
      apply_id,apply_digest,new_custody_id,new_custody_semantic_digest,
      new_custody_source_ref_digest)
    DEFERRABLE INITIALLY DEFERRED
)
"""

CUSTODY_HEADS_FOR_LEAD6_SQL = """
CREATE TABLE lifecycle_rotation_custody_heads(
  project_session_id, run_id, agent_id, custody_id, current_revision,
  state, disposition_code, semantic_digest, source_ref_digest, journal_digest,
  terminal CHECK(terminal IN (0,1)), head_revision,
  PRIMARY KEY(run_id,agent_id,custody_id),
  UNIQUE(project_session_id,run_id,agent_id,custody_id),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id,current_revision,
      state,disposition_code,semantic_digest,source_ref_digest,journal_digest)
    REFERENCES lifecycle_rotation_custody_revisions(
      project_session_id,run_id,agent_id,custody_id,revision,state,
      disposition_code,semantic_digest,source_ref_digest,journal_digest),
  CHECK((terminal=1)=(state='finalized'))
)
"""


def lead6():
    con = _connect()
    cur = con.cursor()

    cur.execute(TRANSITION_APPLIES_SQL)
    cur.execute(RECEIPT_BATCHES_SQL)
    # built for the "no post-state custody row required" assertion below --
    # never populated in this scenario.
    cur.execute(CUSTODY_REVISIONS_FOR_LEAD6_SQL)
    cur.execute(CUSTODY_HEADS_FOR_LEAD6_SQL)

    cur.execute(FRESH_HANDOFFS_STUB_SQL)
    cur.execute(RECEIPT_BATCH_AUTHORIZATIONS_STUB_SQL)
    cur.execute(REVIEW_ADOPTION_RESERVATIONS_STUB_SQL)
    cur.execute(RECOVERY_RETIREMENT_PLANS_STUB_SQL)
    con.commit()

    RUN = "run-6"; AGENT = "agent-6"; PSID = "ps-6"
    BATCH = "batch-6"; APPLY = "apply-6"
    REPLAY = "replay-6"; MUTPLAN = "mutplan-6"

    cur.execute("""
    INSERT INTO lifecycle_receipt_batches
      (batch_id, planned_apply_id, project_session_id, run_id, agent_id,
       transition_kind, effects_set_digest, mutation_plan_digest,
       transition_replay_json, transition_replay_digest, ordered_subject_set_digest,
       receipt_intent_count, review_adoption_reservation_id,
       review_adoption_reservation_digest, fresh_handoff_id, fresh_handoff_digest,
       recovery_retirement_id, recovery_retirement_plan_digest, created_at)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """, (BATCH, APPLY, PSID, RUN, AGENT, "custody-terminal", "eff-6", MUTPLAN,
          "{}", REPLAY, "subj-6", 1, None, None, None, None,
          None, None, "t0"))

    cur.execute("""
    INSERT INTO lifecycle_receipt_batch_authorizations VALUES (?,?,?,?)
    """, (BATCH, "completion-6", "authset-6", "checkpoint-6"))

    # bare terminal apply row -- no custody revision/head row is ever inserted
    cur.execute("""
    INSERT INTO lifecycle_transition_applies
      (apply_id, apply_kind, receipt_batch_id, batch_completion_digest,
       transition_replay_digest, ordered_authority_receipt_set_digest,
       verified_scope_checkpoint_digest, applied_mutation_plan_digest,
       fresh_handoff_id, fresh_handoff_digest, fresh_project_session_id,
       fresh_run_id, fresh_agent_id, fresh_source_mode, fresh_apply_plan_digest,
       new_custody_id, new_custody_semantic_digest, new_custody_source_ref_digest,
       fresh_generation_loss_id, fresh_generation_loss_after_revision,
       fresh_generation_loss_after_semantic_digest,
       fresh_generation_loss_after_source_ref_digest,
       local_write_set_digest, apply_json, apply_digest, applied_at)
    VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)
    """, (APPLY, "terminal", BATCH, "completion-6", REPLAY, "authset-6",
          "checkpoint-6", MUTPLAN, None, None, None, None, None, None, None,
          None, None, None, None, None, None, None, "lw-6", "{}",
          "digest-apply-6", "t0"))

    con.commit()

    apply_row = cur.execute(
        "SELECT apply_kind FROM lifecycle_transition_applies WHERE apply_id=?",
        (APPLY,)).fetchone()
    n_rev = cur.execute(
        "SELECT COUNT(*) FROM lifecycle_rotation_custody_revisions "
        "WHERE run_id=? AND agent_id=?", (RUN, AGENT)).fetchone()[0]
    n_heads = cur.execute(
        "SELECT COUNT(*) FROM lifecycle_rotation_custody_heads "
        "WHERE run_id=? AND agent_id=?", (RUN, AGENT)).fetchone()[0]

    assert apply_row is not None and apply_row[0] == "terminal"
    assert n_rev == 0
    assert n_heads == 0

    con.close()
    return "ACCEPTED", (
        "a valid apply_kind='terminal' lifecycle_transition_applies row "
        "committed with zero rows in lifecycle_rotation_custody_revisions and "
        "lifecycle_rotation_custody_heads for the same (run_id, agent_id): "
        "nothing in the DDL requires the post-state custody rows to exist"
    )


# ---------------------------------------------------------------------------
# LEAD8: fresh-issue single-flight + revocation/handoff ordering.
#
# Focus tables: agent_lifecycle_recovery_capability_issues (~7242-7277),
# agent_lifecycle_recovery_issue_revocations (~7279-7285);
# lifecycle_fresh_recovery_handoffs (~6768-6825) stubbed minimally.
# ---------------------------------------------------------------------------

ISSUE_COLUMNS = [
    "issue_id", "capability_hash", "operator_id", "project_id",
    "project_session_id", "run_id", "agent_id", "session_revision",
    "session_generation", "run_revision", "recovery_source_kind",
    "old_custody_id", "old_action_adapter_id", "old_action_id",
    "old_custody_revision", "generation_loss_id", "generation_loss_revision",
    "recovery_source_ref_digest", "source_journal_digest",
    "checkpoint_digest", "source_provider_session_ref",
    "source_capability_hash", "source_custody_action_id",
    "source_adapter_id", "source_adapter_contract_digest",
    "source_bridge_row_id", "source_bridge_revision",
    "source_provider_generation", "source_principal_generation",
    "source_bridge_generation", "source_project_session_generation",
    "source_run_generation", "source_chair_lease_generation",
    "bridge_owner_kind", "parent_capability_id", "consequential_gate_id",
    "path", "issuance_json", "issuance_digest", "issued_at", "expires_at",
]


def lead8():
    con = _connect()
    cur = con.cursor()

    cur.execute("""
    CREATE TABLE agent_lifecycle_recovery_capability_issues(
      issue_id, capability_hash, operator_id, project_id, project_session_id, run_id,
      agent_id, session_revision, session_generation, run_revision,
      recovery_source_kind, old_custody_id, old_action_adapter_id, old_action_id,
      old_custody_revision,
      generation_loss_id, generation_loss_revision,
      recovery_source_ref_digest, source_journal_digest,
      checkpoint_digest, source_provider_session_ref, source_capability_hash,
      source_custody_action_id, source_adapter_id, source_adapter_contract_digest,
      source_bridge_row_id, source_bridge_revision, source_provider_generation,
      source_principal_generation, source_bridge_generation,
      source_project_session_generation, source_run_generation,
      source_chair_lease_generation, bridge_owner_kind,
      parent_capability_id, consequential_gate_id,
      path CHECK(path='fresh-rotate'), issuance_json, issuance_digest,
      issued_at, expires_at,
      PRIMARY KEY(issue_id), UNIQUE(capability_hash), UNIQUE(issuance_digest),
      UNIQUE(issue_id,project_session_id,run_id,agent_id,
        recovery_source_kind,recovery_source_ref_digest,source_journal_digest),
      CHECK((recovery_source_kind='custody' AND old_custody_id IS NOT NULL AND
          old_custody_revision IS NOT NULL AND generation_loss_id IS NULL AND
          generation_loss_revision IS NULL) OR
        (recovery_source_kind='generation-loss' AND old_custody_id IS NULL AND
          old_custody_revision IS NULL AND generation_loss_id IS NOT NULL AND
          generation_loss_revision IS NOT NULL)),
      FOREIGN KEY(project_session_id,run_id,agent_id,old_custody_id,
          old_custody_revision,recovery_source_ref_digest,source_journal_digest)
        REFERENCES lifecycle_rotation_custody_revisions(
          project_session_id,run_id,agent_id,custody_id,revision,
          source_ref_digest,journal_digest),
      FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
          generation_loss_revision,recovery_source_ref_digest,source_journal_digest)
        REFERENCES lifecycle_generation_loss_revisions(
          project_session_id,run_id,agent_id,generation_loss_id,revision,
          source_ref_digest,journal_digest)
    )
    """)

    cur.execute("""
    CREATE TABLE agent_lifecycle_recovery_issue_revocations(
      issue_id PRIMARY KEY, revocation_kind CHECK(
        revocation_kind IN ('operator-revoked','source-stale')),
      evidence_digest, revoked_at,
      FOREIGN KEY(issue_id)
        REFERENCES agent_lifecycle_recovery_capability_issues(issue_id)
    )
    """)

    # minimal stub of lifecycle_fresh_recovery_handoffs -- only the issue_id
    # linkage relevant to this defect is modelled (~6768-6825 stub minimally)
    cur.execute("""
    CREATE TABLE lifecycle_fresh_recovery_handoffs(
      handoff_id PRIMARY KEY, issue_id UNIQUE,
      project_session_id, run_id, agent_id,
      recovery_source_kind, recovery_source_ref_digest, source_journal_digest,
      FOREIGN KEY(issue_id,project_session_id,run_id,agent_id,
          recovery_source_kind,recovery_source_ref_digest,source_journal_digest)
        REFERENCES agent_lifecycle_recovery_capability_issues(
          issue_id,project_session_id,run_id,agent_id,
          recovery_source_kind,recovery_source_ref_digest,source_journal_digest)
    )
    """)

    # minimal stub parents for the issues table's own FKs
    cur.execute("""
    CREATE TABLE lifecycle_rotation_custody_revisions(
      project_session_id, run_id, agent_id, custody_id, revision,
      source_ref_digest, journal_digest,
      UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
        source_ref_digest,journal_digest)
    )
    """)
    cur.execute("""
    CREATE TABLE lifecycle_generation_loss_revisions(
      project_session_id, run_id, agent_id, generation_loss_id, revision,
      source_ref_digest, journal_digest,
      UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,revision,
        source_ref_digest,journal_digest)
    )
    """)
    con.commit()

    RUN = "run-8"; AGENT = "agent-8"; PSID = "ps-8"
    CUSTODY = "custody-8"; SRC_DIGEST = "src-8"; JOURNAL = "journal-8"

    cur.execute("""
    INSERT INTO lifecycle_rotation_custody_revisions VALUES (?,?,?,?,?,?,?)
    """, (PSID, RUN, AGENT, CUSTODY, 1, SRC_DIGEST, JOURNAL))
    con.commit()

    def insert_issue(issue_id, cap_hash, iss_digest):
        values = {c: None for c in ISSUE_COLUMNS}
        values.update(
            issue_id=issue_id, capability_hash=cap_hash, operator_id="operator-1",
            project_id="proj-8", project_session_id=PSID, run_id=RUN,
            agent_id=AGENT, session_revision=1, session_generation=1,
            run_revision=1, recovery_source_kind="custody",
            old_custody_id=CUSTODY, old_custody_revision=1,
            recovery_source_ref_digest=SRC_DIGEST, source_journal_digest=JOURNAL,
            checkpoint_digest="ckpt-8", source_provider_session_ref="psr-8",
            source_capability_hash="scaphash-8", path="fresh-rotate",
            issuance_json="{}", issuance_digest=iss_digest, issued_at="t0",
            expires_at="t9",
        )
        placeholders = ",".join("?" for _ in ISSUE_COLUMNS)
        sql = (
            "INSERT INTO agent_lifecycle_recovery_capability_issues "
            f"({','.join(ISSUE_COLUMNS)}) VALUES ({placeholders})"
        )
        cur.execute(sql, [values[c] for c in ISSUE_COLUMNS])

    # ---------------- 8a: source single-flight ----------------
    # two independent issues (I1, I2) against the IDENTICAL
    # (project_session_id, run_id, agent_id, recovery_source_kind,
    #  recovery_source_ref_digest) source tuple.
    insert_issue("issue-1", "caphash-1", "issdig-1")
    insert_issue("issue-2", "caphash-2", "issdig-2")
    con.commit()

    n_same_source = cur.execute("""
        SELECT COUNT(*) FROM agent_lifecycle_recovery_capability_issues
        WHERE project_session_id=? AND run_id=? AND agent_id=?
          AND recovery_source_kind='custody' AND recovery_source_ref_digest=?
    """, (PSID, RUN, AGENT, SRC_DIGEST)).fetchone()[0]
    assert n_same_source == 2, "expected both same-source issues accepted"

    # ---------------- 8b: revoke-then-handoff (on issue-1) ----------------
    cur.execute("""
    INSERT INTO agent_lifecycle_recovery_issue_revocations
      (issue_id, revocation_kind, evidence_digest, revoked_at)
    VALUES (?,?,?,?)
    """, ("issue-1", "operator-revoked", "ev-revoke-1", "t1"))
    cur.execute("""
    INSERT INTO lifecycle_fresh_recovery_handoffs
      (handoff_id, issue_id, project_session_id, run_id, agent_id,
       recovery_source_kind, recovery_source_ref_digest, source_journal_digest)
    VALUES (?,?,?,?,?,?,?,?)
    """, ("handoff-for-1", "issue-1", PSID, RUN, AGENT, "custody", SRC_DIGEST,
          JOURNAL))
    con.commit()

    # ---------------- 8c: handoff-then-revoke (on issue-2) ----------------
    cur.execute("""
    INSERT INTO lifecycle_fresh_recovery_handoffs
      (handoff_id, issue_id, project_session_id, run_id, agent_id,
       recovery_source_kind, recovery_source_ref_digest, source_journal_digest)
    VALUES (?,?,?,?,?,?,?,?)
    """, ("handoff-for-2", "issue-2", PSID, RUN, AGENT, "custody", SRC_DIGEST,
          JOURNAL))
    cur.execute("""
    INSERT INTO agent_lifecycle_recovery_issue_revocations
      (issue_id, revocation_kind, evidence_digest, revoked_at)
    VALUES (?,?,?,?)
    """, ("issue-2", "source-stale", "ev-revoke-2", "t2"))
    con.commit()

    n_revocations = cur.execute(
        "SELECT COUNT(*) FROM agent_lifecycle_recovery_issue_revocations"
    ).fetchone()[0]
    n_handoffs = cur.execute(
        "SELECT COUNT(*) FROM lifecycle_fresh_recovery_handoffs"
    ).fetchone()[0]
    assert n_revocations == 2
    assert n_handoffs == 2

    con.close()
    return "ACCEPTED", (
        "(8a) two issues sharing one (project_session_id,run_id,agent_id,"
        "recovery_source_kind,recovery_source_ref_digest) source tuple both "
        "committed -- no source single-flight constraint exists; "
        "(8b) revoke-then-handoff and (8c) handoff-then-revoke both committed "
        "in either order -- no mutual-exclusion FK/CHECK links revocations to "
        "handoffs"
    )


LEADS = [
    ("LEAD1", lead1, "ACCEPTED"),
    ("LEAD3", lead3, "CONFIRMED"),
    ("LEAD4", lead4, "ACCEPTED"),
    ("LEAD6", lead6, "ACCEPTED"),
    ("LEAD8", lead8, "ACCEPTED"),
]


def main():
    results = []
    for name, fn, expected in LEADS:
        try:
            status, detail = fn()
        except Exception as e:  # keep the script honest: report, don't mask
            status, detail = "REJECTED", (
                f"unexpected {type(e).__name__}: {e}\n" + traceback.format_exc()
            )
        results.append((name, expected, status, detail))

    all_ok = True
    lines = []
    for name, expected, status, detail in results:
        if status == expected:
            if status == "CONFIRMED":
                line = f"{name}: CONFIRMED (prepare-time insert blocked) detail={detail}"
            else:
                line = f"{name}: ACCEPTED (defect reproduced) detail={detail}"
        else:
            all_ok = False
            line = f"{name}: {status} err={detail}"
        print(line)

    with open(RESULTS_PATH, "w") as f:
        for name, expected, status, detail in results:
            f.write(f"{name}={status}\n")

    return 0 if all_ok else 1


if __name__ == "__main__":
    sys.exit(main())
