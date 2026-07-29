#!/usr/bin/env python3
"""
Executable SQLite fixtures proving six substantiated spec defects are
accepted by DDL in the current semantic persistence, workspace-trust,
lifecycle-custody and observability owners. Those current owners contain DDL
derived from the historical freeze at commit d7f3536; Git retains the former
specification path and its full history.

Each defect gets its own fresh in-memory sqlite3 connection with
PRAGMA foreign_keys=ON. Column sets, PRIMARY KEY, UNIQUE and FOREIGN KEY
clauses are transcribed verbatim from the spec's pseudo-DDL blocks (the
anchors are given inline above each table). CHECK constraints are only
carried over when they are the literal subject of the assertion (e.g. the
LEAD7 terminal-parity CHECK, the MF04-3 subject_kind discriminator CHECK,
the LEAD5 ordinal/kind CHECK); other business-logic CHECKs are omitted for
minimality per the fixture brief, which scopes verbatim fidelity to
"column set, PK, UNIQUE, FK". No timestamps/randomness are used anywhere;
all ids/digests are fixed literals.

Run: python3 fixtures_schema.py
Exit 0 iff every defect below reproduces as predicted.
"""
import sqlite3
import sys

RESULTS = []  # list of (id, ok: bool, line: str)


def new_conn():
    conn = sqlite3.connect(":memory:")
    conn.execute("PRAGMA foreign_keys=ON")
    return conn


def record(id_, ok, line):
    RESULTS.append((id_, ok, line))
    print(line)


# ---------------------------------------------------------------------------
# LEAD2 -- invalid retirement FK
# Anchors: lifecycle_recovery_retirement_plans SS6137-6160,
#          lifecycle_rotation_custody_revisions SS5954-5985
# ---------------------------------------------------------------------------
def check_lead2():
    conn = new_conn()
    # Minimal stub parent for the custody_revisions' own FK.
    conn.execute("""
        CREATE TABLE lifecycle_rotation_custodies(
          project_session_id, run_id, agent_id, custody_id,
          PRIMARY KEY(project_session_id,run_id,agent_id,custody_id)
        )
    """)
    # Verbatim column set / PK / UNIQUE set from SS5954-5985. FK to the
    # custodies stub is kept (this is the "valid parent" the brief asks us
    # to insert); other internal self/receipt FKs are dropped as
    # unrelated-parent stubs per the fixture brief.
    conn.execute("""
        CREATE TABLE lifecycle_rotation_custody_revisions(
          project_session_id, run_id, agent_id, custody_id,
          revision, prior_revision, prior_journal_digest,
          state, disposition_code, proof_kind,
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
            semantic_digest,source_ref_digest,journal_digest,
            origin_fresh_apply_id,origin_fresh_apply_digest),
          UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,state,
            disposition_code,semantic_digest,source_ref_digest,journal_digest),
          UNIQUE(semantic_digest), UNIQUE(source_ref_digest), UNIQUE(journal_digest),
          FOREIGN KEY(project_session_id,run_id,agent_id,custody_id)
            REFERENCES lifecycle_rotation_custodies(
              project_session_id,run_id,agent_id,custody_id)
        )
    """)
    # Verbatim column set / PK / UNIQUE set from SS6137-6160, plus the FK
    # under test (lines ~6155-6159).
    conn.execute("""
        CREATE TABLE lifecycle_recovery_retirement_plans(
          retirement_id, revision,
          project_session_id, run_id, agent_id, custody_id, custody_revision,
          custody_source_ref_digest, custody_journal_digest,
          finalized_disposition,
          finalized_terminal_evidence_digest, admission_digest,
          transition_proof_json, transition_proof_digest,
          mutation_plan_json, mutation_plan_digest, retirement_evidence_digest,
          planned_apply_id, recorded_at, plan_json, retirement_plan_digest,
          PRIMARY KEY(retirement_id),
          UNIQUE(planned_apply_id),
          UNIQUE(retirement_plan_digest),
          UNIQUE(retirement_id,revision,retirement_plan_digest),
          UNIQUE(retirement_id,retirement_plan_digest,planned_apply_id),
          UNIQUE(project_session_id,run_id,agent_id,custody_id,custody_revision),
          UNIQUE(retirement_id,retirement_plan_digest,planned_apply_id,
            project_session_id,run_id,agent_id,mutation_plan_digest),
          UNIQUE(retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
            custody_id,custody_revision,custody_source_ref_digest,
            custody_journal_digest,finalized_disposition,retirement_plan_digest),
          FOREIGN KEY(project_session_id,run_id,agent_id,custody_id,custody_revision,
              finalized_disposition,custody_source_ref_digest,custody_journal_digest)
            REFERENCES lifecycle_rotation_custody_revisions(
              project_session_id,run_id,agent_id,custody_id,revision,
              disposition_code,source_ref_digest,journal_digest)
        )
    """)

    conn.execute("INSERT INTO lifecycle_rotation_custodies VALUES ('ps1','run1','agent1','custodyA')")
    conn.execute("""
        INSERT INTO lifecycle_rotation_custody_revisions VALUES(
          'ps1','run1','agent1','custodyA',
          1, NULL, NULL,
          'finalized','no-effect','confirmed-abandon',
          'term-ev-1',
          '{}','sem-1','src-1',
          NULL, NULL,
          NULL, NULL, NULL,
          '{}','jrn-1', 1)
    """)
    conn.commit()

    try:
        conn.execute("""
            INSERT INTO lifecycle_recovery_retirement_plans VALUES(
              'ret-1', 1,
              'ps1','run1','agent1','custodyA', 1,
              'src-1','jrn-1',
              'no-effect',
              'term-ev-1', 'adm-1',
              '{}', 'proof-digest-1',
              '{}', 'mutation-digest-1', 'ret-ev-1',
              'apply-1', 1, '{}', 'ret-plan-digest-1')
        """)
        conn.commit()
        record("LEAD2", False,
               "LEAD2: REJECTED err=<no error raised; INSERT unexpectedly succeeded -- defect NOT reproduced>")
        return False
    except sqlite3.OperationalError as e:
        msg = str(e)
        expected = (
            'foreign key mismatch - "lifecycle_recovery_retirement_plans" '
            'referencing "lifecycle_rotation_custody_revisions"'
        )
        if msg == expected:
            record("LEAD2", True,
                   "LEAD2: CONFIRMED (foreign key mismatch — FK targets non-unique tuple)")
            return True
        record("LEAD2", False, f"LEAD2: REJECTED err={msg} (unexpected error text)")
        return False
    finally:
        conn.close()


# ---------------------------------------------------------------------------
# LEAD5 -- effect-set closure
# Anchors: lifecycle_receipt_intents SS6292-6317,
#          lifecycle_receipt_custody_effects SS6212-6233,
#          lifecycle_receipt_batches SS6162-6210 (minimal),
#          lifecycle_receipt_batch_completions SS6357-6419
# ---------------------------------------------------------------------------
def build_lead5_schema(conn):
    # lifecycle_receipt_batches -- minimal (columns/PK/UNIQUE verbatim from
    # SS6162-6210; FKs to review_adoption_reservations / fresh_recovery_handoffs
    # / recovery_retirement_plans are unrelated parents, not stubbed, since
    # those FK columns stay NULL in every row we insert here).
    conn.execute("""
        CREATE TABLE lifecycle_receipt_batches(
          batch_id, planned_apply_id,
          project_session_id, run_id, agent_id,
          transition_kind,
          effects_set_digest, mutation_plan_digest,
          transition_replay_json, transition_replay_digest,
          ordered_subject_set_digest,
          receipt_intent_count,
          review_adoption_reservation_id, review_adoption_reservation_digest,
          fresh_handoff_id, fresh_handoff_digest,
          recovery_retirement_id, recovery_retirement_plan_digest, created_at,
          PRIMARY KEY(batch_id),
          UNIQUE(planned_apply_id),
          UNIQUE(project_session_id,run_id,agent_id,transition_replay_digest),
          UNIQUE(batch_id,planned_apply_id),
          UNIQUE(batch_id,transition_kind,receipt_intent_count),
          UNIQUE(batch_id,planned_apply_id,transition_replay_digest,mutation_plan_digest),
          UNIQUE(batch_id,project_session_id,run_id),
          UNIQUE(batch_id,project_session_id,run_id,agent_id),
          UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id),
          UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
            review_adoption_reservation_digest),
          UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
            transition_kind),
          UNIQUE(batch_id,planned_apply_id,transition_replay_digest,
            mutation_plan_digest,fresh_handoff_id,fresh_handoff_digest)
        )
    """)
    # lifecycle_receipt_custody_effects -- verbatim column set/PK/UNIQUE from
    # SS6212-6233. Only the FK to lifecycle_receipt_batches is kept; the FK to
    # lifecycle_rotation_custody_revisions (pre_revision/pre_journal_digest) is
    # an unrelated parent for this identity-mismatch test and is dropped.
    conn.execute("""
        CREATE TABLE lifecycle_receipt_custody_effects(
          batch_id, ordinal, role, transition_kind,
          planned_apply_id, project_session_id, run_id, agent_id, custody_id,
          pre_revision, pre_journal_digest,
          final_revision, final_semantic_digest, final_source_ref_digest,
          effect_digest,
          PRIMARY KEY(batch_id,ordinal), UNIQUE(batch_id), UNIQUE(effect_digest),
          UNIQUE(batch_id,effect_digest),
          UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
            custody_id,final_revision,final_semantic_digest,final_source_ref_digest),
          FOREIGN KEY(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
              transition_kind)
            REFERENCES lifecycle_receipt_batches(
              batch_id,planned_apply_id,project_session_id,run_id,agent_id,
              transition_kind)
        )
    """)
    # lifecycle_receipt_intents -- verbatim column set/PK/UNIQUE from
    # SS6292-6317, plus the ordinal/kind CHECK (kept because it is the
    # control that shows the closure gap is real: this CHECK ties `kind` to
    # `batch_transition_kind` but says nothing about `subject_owner_kind`).
    conn.execute("""
        CREATE TABLE lifecycle_receipt_intents(
          batch_id, ordinal,
          batch_transition_kind, batch_intent_count,
          kind,
          project_session_id, run_id, agent_id,
          subject_owner_kind,
          subject_owner_id, subject_owner_revision,
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
          CHECK((ordinal=1 AND kind=batch_transition_kind) OR
            (ordinal=2 AND batch_transition_kind='custody-terminal' AND
              batch_intent_count=2 AND kind='review-adoption-decision'))
        )
    """)
    # lifecycle_receipt_batch_completions -- verbatim column set/PK/UNIQUE
    # from SS6357-6419 (built per the fixture brief; not exercised by the
    # 5a/5b assertions, so only the FKs to tables we actually stub are kept).
    conn.execute("""
        CREATE TABLE lifecycle_receipt_batch_completions(
          batch_id, transition_kind, receipt_intent_count,
          ordinal_one, ordinal_one_intent_digest,
          ordinal_one_subject_digest,
          ordinal_one_receipt_digest,
          ordinal_two, ordinal_two_intent_digest, ordinal_two_subject_digest,
          ordinal_two_receipt_digest,
          primary_custody_effect_digest,
          primary_loss_effect_role,
          primary_loss_effect_digest, primary_retirement_effect_digest,
          linked_loss_effect_role,
          linked_loss_effect_digest, ordered_authority_receipt_set_digest,
          completion_json, completion_digest, completed_at,
          PRIMARY KEY(batch_id),
          UNIQUE(batch_id,completion_digest,ordered_authority_receipt_set_digest),
          FOREIGN KEY(batch_id,transition_kind,receipt_intent_count)
            REFERENCES lifecycle_receipt_batches(
              batch_id,transition_kind,receipt_intent_count),
          FOREIGN KEY(batch_id,primary_custody_effect_digest)
            REFERENCES lifecycle_receipt_custody_effects(batch_id,effect_digest)
        )
    """)


def check_lead5():
    conn = new_conn()
    build_lead5_schema(conn)

    # Common batch row: custody-terminal, 1 intent.
    conn.execute("""
        INSERT INTO lifecycle_receipt_batches VALUES(
          'batch-1','apply-1','ps1','run1','agent1',
          'custody-terminal',
          'effects-digest-1','mutation-digest-1',
          '{}','replay-digest-1',
          'subj-set-digest-1',
          1,
          NULL, NULL,
          NULL, NULL,
          NULL, NULL, 1)
    """)
    conn.commit()

    ok_all = True

    # 5a: wrong-owner intent -- kind='custody-terminal' but
    # subject_owner_kind='generation-loss'. No CHECK ties kind to
    # subject_owner_kind, so this must be ACCEPTED.
    try:
        conn.execute("""
            INSERT INTO lifecycle_receipt_intents VALUES(
              'batch-1', 1,
              'custody-terminal', 1,
              'custody-terminal',
              'ps1','run1','agent1',
              'generation-loss',
              'custodyA', 5,
              '{}', 'subj-digest-5a', 'intent-digest-5a', 1)
        """)
        conn.commit()
        print("LEAD5a: ACCEPTED (defect reproduced -- wrong-owner intent has no kind<->owner CHECK)")
    except sqlite3.OperationalError as e:
        print(f"LEAD5a: REJECTED err={e} (defect NOT reproduced)")
        ok_all = False

    # 5b: anti-extra/mismatch -- for the same batch (whose ordinal-1 intent
    # names custody A rev 5 above), insert a custody effect naming custody B
    # rev 9 instead. No intent->effect identity FK exists, so this must be
    # ACCEPTED.
    try:
        conn.execute("""
            INSERT INTO lifecycle_receipt_custody_effects VALUES(
              'batch-1', 1, 'primary', 'custody-terminal',
              'apply-1','ps1','run1','agent1','custodyB',
              8, 'pre-jrn-custodyB-8',
              9, 'sem-9', 'src-9',
              'effect-digest-5b')
        """)
        conn.commit()
        print("LEAD5b: ACCEPTED (defect reproduced -- effect names a different custody/revision than the batch's intent, no identity FK)")
    except sqlite3.OperationalError as e:
        print(f"LEAD5b: REJECTED err={e} (defect NOT reproduced)")
        ok_all = False

    conn.close()
    if ok_all:
        record("LEAD5", True, "LEAD5: ACCEPTED (defect reproduced)")
    else:
        record("LEAD5", False, "LEAD5: REJECTED (one or both sub-cases did not reproduce)")
    return ok_all


# ---------------------------------------------------------------------------
# LEAD7 -- lying heads
# Anchors: lifecycle_receipt_scope_checkpoints SS6056-6077,
#          lifecycle_receipt_scope_heads SS6079-6094,
#          lifecycle_generation_loss_revisions SS7094-7199,
#          lifecycle_generation_loss_heads SS7201-7223
# ---------------------------------------------------------------------------
def check_lead7():
    conn = new_conn()

    # --- 7a: scope checkpoint / scope head -----------------------------
    conn.execute("""
        CREATE TABLE lifecycle_admitted_run_scopes(
          project_id, project_session_id, run_id, authority_id,
          admission_digest, admitted_at,
          PRIMARY KEY(project_session_id,run_id),
          UNIQUE(project_id,project_session_id,run_id),
          UNIQUE(project_session_id,run_id,authority_id)
        )
    """)
    conn.execute("""
        CREATE TABLE lifecycle_receipt_scope_checkpoints(
          project_session_id, run_id, authority_id,
          receipt_count,
          head_authority_sequence,
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
          FOREIGN KEY(project_session_id,run_id,authority_id)
            REFERENCES lifecycle_admitted_run_scopes(
              project_session_id,run_id,authority_id)
        )
    """)
    conn.execute("""
        CREATE TABLE lifecycle_receipt_scope_heads(
          project_session_id, run_id, authority_id, receipt_count,
          head_authority_sequence, head_receipt_digest,
          ordered_record_set_digest, checkpoint_digest, revision,
          PRIMARY KEY(project_session_id,run_id),
          FOREIGN KEY(project_session_id,run_id,checkpoint_digest)
            REFERENCES lifecycle_receipt_scope_checkpoints(
              project_session_id,run_id,checkpoint_digest),
          FOREIGN KEY(project_session_id,run_id,authority_id,receipt_count,
              head_authority_sequence,head_receipt_digest,ordered_record_set_digest,
              checkpoint_digest)
            REFERENCES lifecycle_receipt_scope_checkpoints(
              project_session_id,run_id,authority_id,receipt_count,
              head_authority_sequence,head_receipt_digest,ordered_record_set_digest,
              checkpoint_digest)
        )
    """)

    conn.execute("INSERT INTO lifecycle_admitted_run_scopes VALUES('proj1','ps1','run1','auth1','adm-1',1)")
    conn.execute("""
        INSERT INTO lifecycle_receipt_scope_checkpoints VALUES(
          'ps1','run1','auth1',
          5,
          5,
          'hrd-5','ord-5',
          '{}','ckpt-5','att-1',1)
    """)
    conn.commit()

    ok_all = True
    try:
        # Core FK (project_session_id,run_id,checkpoint_digest) truthfully
        # matches the real checkpoint row ('ps1','run1','ckpt-5'). The fuller
        # FK includes head_receipt_digest, which we set NULL here -- per
        # SQLite's MATCH SIMPLE semantics any NULL column in a composite FK
        # makes the whole FK vacuous, so the lying receipt_count=0/
        # head_receipt_digest=NULL is never checked against the real
        # checkpoint's receipt_count=5/head_receipt_digest='hrd-5'.
        conn.execute("""
            INSERT INTO lifecycle_receipt_scope_heads VALUES(
              'ps1','run1','auth1',
              0,
              0, NULL,
              NULL, 'ckpt-5', 1)
        """)
        conn.commit()
        print("LEAD7a: ACCEPTED (defect reproduced -- fuller FK null-vacuous, no head-local parity check)")
    except sqlite3.OperationalError as e:
        print(f"LEAD7a: REJECTED err={e} (defect NOT reproduced)")
        ok_all = False

    # --- 7b: generation-loss revision / head ----------------------------
    conn.execute("""
        CREATE TABLE lifecycle_generation_losses(
          project_session_id, run_id, agent_id, generation_loss_id,
          PRIMARY KEY(project_session_id,run_id,agent_id,generation_loss_id)
        )
    """)
    conn.execute("""
        CREATE TABLE lifecycle_generation_loss_revisions(
          project_session_id, run_id, agent_id, generation_loss_id,
          revision, prior_revision, prior_journal_digest,
          state,
          abandon_kind_code,
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
          FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id)
            REFERENCES lifecycle_generation_losses(
              project_session_id,run_id,agent_id,generation_loss_id)
        )
    """)
    conn.execute("""
        CREATE TABLE lifecycle_generation_loss_heads(
          project_session_id, run_id, agent_id, generation_loss_id, current_revision,
          state, abandon_kind_code, recovery_action_adapter_id, recovery_action_id,
          active_recovery_custody_id, semantic_digest, source_ref_digest,
          journal_digest, terminal, head_revision,
          PRIMARY KEY(run_id,agent_id,generation_loss_id),
          UNIQUE(project_session_id,run_id,agent_id,generation_loss_id),
          FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
              current_revision,semantic_digest,source_ref_digest,journal_digest)
            REFERENCES lifecycle_generation_loss_revisions(
              project_session_id,run_id,agent_id,generation_loss_id,revision,
              semantic_digest,source_ref_digest,journal_digest),
          FOREIGN KEY(project_session_id,run_id,agent_id,generation_loss_id,
              current_revision,state,abandon_kind_code,recovery_action_adapter_id,
              recovery_action_id,active_recovery_custody_id,semantic_digest,
              source_ref_digest,journal_digest)
            REFERENCES lifecycle_generation_loss_revisions(
              project_session_id,run_id,agent_id,generation_loss_id,revision,state,
              abandon_kind_code,recovery_action_adapter_id,recovery_action_id,
              active_recovery_custody_id,semantic_digest,source_ref_digest,
              journal_digest),
          CHECK((terminal=1)=(state IN ('recovered-adopted','abandoned')))
        )
    """)

    conn.execute("INSERT INTO lifecycle_generation_losses VALUES('ps1','run1','agent1','loss1')")
    conn.execute("""
        INSERT INTO lifecycle_generation_loss_revisions VALUES(
          'ps1','run1','agent1','loss1',
          1, NULL, NULL,
          'open',
          'none',
          NULL, NULL, NULL,
          NULL, '{}','sem-open','src-open',
          NULL, NULL,
          NULL, NULL, NULL,
          '{}','jrn-open', 1)
    """)
    conn.commit()

    try:
        # Core FK (...,current_revision,semantic_digest,source_ref_digest,
        # journal_digest) truthfully matches the real 'open' revision row.
        # The fuller FK additionally carries recovery_action_adapter_id
        # (NULL here), so it is vacuous -- the head is free to claim
        # state='abandoned'/terminal=1 even though the referenced revision
        # is actually 'open'. The terminal-parity CHECK only inspects the
        # head's own columns (terminal=1, state='abandoned') so it passes.
        conn.execute("""
            INSERT INTO lifecycle_generation_loss_heads VALUES(
              'ps1','run1','agent1','loss1', 1,
              'abandoned', 'direct-open', NULL, NULL,
              NULL, 'sem-open', 'src-open',
              'jrn-open', 1, 1)
        """)
        conn.commit()
        print("LEAD7b: ACCEPTED (defect reproduced -- fuller FK null-vacuous, terminal-parity CHECK is head-local only)")
    except sqlite3.OperationalError as e:
        print(f"LEAD7b: REJECTED err={e} (defect NOT reproduced)")
        ok_all = False

    conn.close()
    if ok_all:
        record("LEAD7", True, "LEAD7: ACCEPTED (defect reproduced)")
    else:
        record("LEAD7", False, "LEAD7: REJECTED (one or both sub-cases did not reproduce)")
    return ok_all


# ---------------------------------------------------------------------------
# MF04-1 -- operator_git_grants invalid FK (P0)
# Anchors: run_authority_revisions prose SS997-1017 (4-col UNIQUE at
#          SS1008-1011), operator_git_grants SS1041-1083 (FK at SS1071-1076)
# ---------------------------------------------------------------------------
def check_mf04_1():
    conn = new_conn()
    conn.execute("CREATE TABLE runs(project_session_id, run_id, PRIMARY KEY(project_session_id,run_id))")
    conn.execute("CREATE TABLE git_execution_profiles(profile_id, revision, PRIMARY KEY(profile_id,revision))")
    # run_authority_revisions: PK per SS1007-1008, and ONLY the 4-col UNIQUE
    # the spec states at SS1008-1011. No 6-col unique/PK is declared anywhere
    # in the spec for this table -- that is precisely the defect.
    conn.execute("""
        CREATE TABLE run_authority_revisions(
          project_session_id TEXT NOT NULL,
          coordination_run_id TEXT NOT NULL,
          authority_revision INTEGER NOT NULL,
          authority_ref TEXT NOT NULL,
          git_allowlist_epoch INTEGER NOT NULL,
          git_allowlist_digest TEXT NOT NULL,
          activated_at_run_revision INTEGER,
          created_at INTEGER NOT NULL,
          PRIMARY KEY(project_session_id, coordination_run_id, authority_revision),
          UNIQUE(project_session_id, coordination_run_id, authority_revision, authority_ref)
        )
    """)
    # operator_git_grants: verbatim from the spec's own ```sql block
    # (SS1041-1083), minus the two FKs to runs/git_execution_profiles which
    # are unrelated to the FK under test (kept as plain columns, stub
    # parents created above but not wired since they don't bear on the
    # defect).
    conn.execute("""
        CREATE TABLE operator_git_grants (
          grant_id TEXT NOT NULL,
          revision INTEGER NOT NULL CHECK (revision >= 1),
          project_id TEXT NOT NULL,
          project_session_id TEXT NOT NULL,
          session_generation INTEGER NOT NULL CHECK (session_generation >= 1),
          issuing_session_revision INTEGER NOT NULL CHECK (issuing_session_revision >= 1),
          coordination_run_id TEXT NOT NULL,
          issuing_run_revision INTEGER NOT NULL CHECK (issuing_run_revision >= 1),
          issuing_dependency_revision INTEGER NOT NULL CHECK (issuing_dependency_revision >= 1),
          authority_ref TEXT NOT NULL,
          authority_revision INTEGER NOT NULL CHECK (authority_revision >= 1),
          git_allowlist_epoch INTEGER NOT NULL CHECK (git_allowlist_epoch >= 1),
          git_allowlist_digest TEXT NOT NULL,
          repository_root TEXT NOT NULL,
          worktree_path TEXT NOT NULL,
          execution_profile_id TEXT NOT NULL,
          execution_profile_revision INTEGER NOT NULL CHECK (execution_profile_revision >= 1),
          execution_profile_digest TEXT NOT NULL,
          source_kind TEXT NOT NULL CHECK (source_kind IN ('launch-envelope','operator-command')),
          source_digest TEXT NOT NULL,
          constraints_json TEXT NOT NULL,
          grant_digest TEXT NOT NULL,
          state TEXT NOT NULL CHECK (state IN ('active','revoked')),
          expires_at INTEGER NOT NULL,
          created_at INTEGER NOT NULL,
          revoked_at INTEGER,
          PRIMARY KEY (grant_id, revision),
          FOREIGN KEY (project_session_id, coordination_run_id,
                       authority_revision, authority_ref,
                       git_allowlist_epoch, git_allowlist_digest)
            REFERENCES run_authority_revisions(
              project_session_id, coordination_run_id, authority_revision,
              authority_ref, git_allowlist_epoch, git_allowlist_digest),
          CHECK (length(authority_ref)=71 AND substr(authority_ref,1,7)='sha256:'),
          CHECK (length(grant_digest)=71 AND substr(grant_digest,1,7)='sha256:'),
          CHECK ((state='active' AND revoked_at IS NULL) OR
                 (state<>'active' AND revoked_at IS NOT NULL))
        )
    """)

    conn.execute("INSERT INTO runs VALUES('ps1','run1')")
    conn.execute("INSERT INTO git_execution_profiles VALUES('profile1',1)")

    sha_ref = "sha256:" + ("a" * 64)
    sha_digest = "sha256:" + ("b" * 64)

    conn.execute(f"""
        INSERT INTO run_authority_revisions VALUES(
          'ps1','run1',1,'{sha_ref}',1,'digestA',1,1)
    """)
    conn.commit()

    try:
        conn.execute(f"""
            INSERT INTO operator_git_grants VALUES(
              'grant-1', 1, 'proj1', 'ps1', 1, 1,
              'run1', 1, 1,
              '{sha_ref}', 1, 1, 'digestA',
              '/repo', '/repo/wt', 'profile1', 1, 'exec-digest-1',
              'launch-envelope', 'source-digest-1', '{{}}',
              '{sha_digest}', 'active', 9999999999, 1, NULL)
        """)
        conn.commit()
        record("MF04-1", False,
               "MF04-1: REJECTED err=<no error raised; INSERT unexpectedly succeeded -- defect NOT reproduced>")
        return False
    except sqlite3.OperationalError as e:
        msg = str(e)
        expected = (
            'foreign key mismatch - "operator_git_grants" referencing '
            '"run_authority_revisions"'
        )
        if msg == expected:
            record("MF04-1", True, "MF04-1: CONFIRMED (foreign key mismatch — FK targets 6-col tuple with only a 4-col UNIQUE)")
            return True
        record("MF04-1", False, f"MF04-1: REJECTED err={msg} (unexpected error text)")
        return False
    finally:
        conn.close()


# ---------------------------------------------------------------------------
# MF04-2 -- smoke same-adapter
# Anchor: adapter_provider_smoke_subjects SS7731-7741
# ---------------------------------------------------------------------------
def check_mf04_2():
    conn = new_conn()
    conn.execute("CREATE TABLE artifacts(artifact_id, revision, PRIMARY KEY(artifact_id,revision))")
    conn.execute("CREATE TABLE provider_action_pair_preflights(adapter_id, action_id, PRIMARY KEY(adapter_id,action_id))")
    conn.execute("""
        CREATE TABLE adapter_provider_smoke_subjects(
          adapter_id, smoke_id, action_adapter_id, action_id,
          evidence_id, evidence_revision, created_at,
          PRIMARY KEY(adapter_id, smoke_id),
          UNIQUE(action_adapter_id, action_id),
          UNIQUE(evidence_id, evidence_revision),
          FOREIGN KEY(evidence_id, evidence_revision)
            REFERENCES artifacts(artifact_id, revision),
          FOREIGN KEY(action_adapter_id, action_id)
            REFERENCES provider_action_pair_preflights(adapter_id, action_id)
        )
    """)

    conn.execute("INSERT INTO artifacts VALUES('art1',1)")
    # provider_action_pair_preflights is populated ONLY for adapterB.
    conn.execute("INSERT INTO provider_action_pair_preflights VALUES('adapterB','actionX')")
    conn.commit()

    try:
        conn.execute("""
            INSERT INTO adapter_provider_smoke_subjects VALUES(
              'adapterA', 'smoke1', 'adapterB', 'actionX', 'art1', 1, 1)
        """)
        conn.commit()
        record("MF04-2", True,
               "MF04-2: ACCEPTED (defect reproduced — no CHECK(action_adapter_id=adapter_id); smoke row for adapterA cites adapterB's action pair)")
        return True
    except sqlite3.OperationalError as e:
        record("MF04-2", False, f"MF04-2: REJECTED err={e} (defect NOT reproduced)")
        return False
    finally:
        conn.close()


# ---------------------------------------------------------------------------
# MF04-3 -- activation-config unenforced
# Anchor: adapter_effective_configurations SS7743-7801
# ---------------------------------------------------------------------------
def check_mf04_3():
    conn = new_conn()
    conn.execute("CREATE TABLE artifacts(artifact_id, revision, PRIMARY KEY(artifact_id,revision))")
    conn.execute("""
        CREATE TABLE adapter_capability_snapshots(
          adapter_id, snapshot_generation, snapshot_digest, capability_body_digest,
          UNIQUE(adapter_id, snapshot_generation, snapshot_digest, capability_body_digest)
        )
    """)
    conn.execute("""
        CREATE TABLE discovery_surface_manifests(
          evidence_id, evidence_revision,
          PRIMARY KEY(evidence_id, evidence_revision)
        )
    """)
    conn.execute("""
        CREATE TABLE adapter_activation_subjects(
          adapter_id, activation_id, activation_revision,
          PRIMARY KEY(adapter_id, activation_id, activation_revision)
        )
    """)
    conn.execute("""
        CREATE TABLE adapter_provider_smoke_subjects(
          adapter_id, smoke_id,
          PRIMARY KEY(adapter_id, smoke_id)
        )
    """)
    conn.execute("CREATE TABLE provider_action_pair_preflights(adapter_id, action_id, PRIMARY KEY(adapter_id,action_id))")

    # Verbatim column set / PK / UNIQUE / FK / discriminator CHECK from
    # SS7743-7801 (the discriminator CHECK is kept -- it is the literal
    # subject of the assertion: it constrains subject_activation_id /
    # subject_smoke_id / subject_action_adapter_id / subject_action_id per
    # subject_kind, but never touches activation_configuration_id/_revision/
    # _digest).
    conn.execute("""
        CREATE TABLE adapter_effective_configurations(
          configuration_id, configuration_revision,
          adapter_id TEXT NOT NULL, adapter_contract_digest NOT NULL,
          executable_identity_digest NOT NULL,
          capability_snapshot_generation, capability_snapshot_digest,
          capability_body_digest,
          subject_kind TEXT NOT NULL CHECK(subject_kind IN
            ('activation','provider-smoke','provider-action')),
          subject_ref_digest TEXT NOT NULL,
          subject_activation_id, subject_activation_revision, subject_smoke_id,
          subject_action_adapter_id, subject_action_id,
          activation_configuration_id, activation_configuration_revision,
          activation_configuration_digest, requested_configuration_digest,
          effective_configuration_digest, permission_profile_digest,
          discovery_surface_evidence_id, discovery_surface_evidence_revision,
          evidence_id, evidence_revision,
          configuration_json, configuration_digest, created_at,
          PRIMARY KEY(configuration_id, configuration_revision),
          UNIQUE(configuration_id, configuration_revision, configuration_digest),
          UNIQUE(evidence_id, evidence_revision),
          UNIQUE(configuration_digest),
          UNIQUE(subject_action_adapter_id,subject_action_id,subject_kind,
            adapter_contract_digest,configuration_id,configuration_revision,
            configuration_digest,effective_configuration_digest,
            executable_identity_digest),
          FOREIGN KEY(evidence_id, evidence_revision)
            REFERENCES artifacts(artifact_id, revision),
          FOREIGN KEY(adapter_id, capability_snapshot_generation,
              capability_snapshot_digest, capability_body_digest)
            REFERENCES adapter_capability_snapshots(
              adapter_id, snapshot_generation, snapshot_digest,
              capability_body_digest),
          FOREIGN KEY(discovery_surface_evidence_id,
              discovery_surface_evidence_revision)
            REFERENCES discovery_surface_manifests(evidence_id, evidence_revision),
          FOREIGN KEY(adapter_id, subject_activation_id, subject_activation_revision)
            REFERENCES adapter_activation_subjects(
              adapter_id, activation_id, activation_revision),
          FOREIGN KEY(adapter_id, subject_smoke_id)
            REFERENCES adapter_provider_smoke_subjects(adapter_id, smoke_id),
          FOREIGN KEY(subject_action_adapter_id, subject_action_id)
            REFERENCES provider_action_pair_preflights(adapter_id, action_id),
          FOREIGN KEY(activation_configuration_id,
              activation_configuration_revision,
              activation_configuration_digest)
            REFERENCES adapter_effective_configurations(
              configuration_id, configuration_revision, configuration_digest),
          CHECK(
            (subject_kind='activation' AND subject_activation_id IS NOT NULL AND
              subject_activation_revision IS NOT NULL AND subject_smoke_id IS NULL AND
              subject_action_adapter_id IS NULL AND subject_action_id IS NULL) OR
            (subject_kind='provider-smoke' AND subject_activation_id IS NULL AND
              subject_activation_revision IS NULL AND subject_smoke_id IS NOT NULL AND
              subject_action_adapter_id IS NULL AND subject_action_id IS NULL) OR
            (subject_kind='provider-action' AND subject_activation_id IS NULL AND
              subject_activation_revision IS NULL AND subject_smoke_id IS NULL AND
              subject_action_adapter_id IS NOT NULL AND
              subject_action_adapter_id=adapter_id AND subject_action_id IS NOT NULL))
        )
    """)

    conn.execute("INSERT INTO adapter_provider_smoke_subjects VALUES('adapterA','smoke1')")
    conn.commit()

    try:
        # subject_kind='provider-smoke' with the activation_configuration_*
        # triple entirely NULL. The discriminator CHECK is satisfied
        # (subject_smoke_id IS NOT NULL, activation/action columns NULL) and
        # says nothing about the triple; the self-FK on the triple is
        # NULL-vacuous. capability_snapshot_* and discovery_surface_* /
        # evidence_* are also left NULL (vacuous FKs, no unrelated stub rows
        # needed).
        conn.execute("""
            INSERT INTO adapter_effective_configurations VALUES(
              'cfg1', 1,
              'adapterA', 'contract-digest-1',
              'exe-digest-1',
              NULL, NULL,
              NULL,
              'provider-smoke',
              'subj-ref-digest-1',
              NULL, NULL, 'smoke1',
              NULL, NULL,
              NULL, NULL,
              NULL, NULL,
              'effective-digest-1', NULL,
              NULL, NULL,
              NULL, NULL,
              '{}', 'cfg-digest-1', 1)
        """)
        conn.commit()
        record("MF04-3", True,
               "MF04-3: ACCEPTED (defect reproduced — discriminator CHECK doesn't constrain the activation_configuration_* triple; all-NULL accepted)")
        return True
    except sqlite3.OperationalError as e:
        record("MF04-3", False, f"MF04-3: REJECTED err={e} (defect NOT reproduced)")
        return False
    finally:
        conn.close()


def main():
    results_path = __file__.rsplit("/", 1)[0] + "/results_schema.txt"

    all_ok = True
    all_ok &= check_lead2()
    all_ok &= check_lead5()
    all_ok &= check_lead7()
    all_ok &= check_mf04_1()
    all_ok &= check_mf04_2()
    all_ok &= check_mf04_3()

    with open(results_path, "w") as f:
        for id_, ok, line in RESULTS:
            f.write(line.split(": ", 1)[0] + "=" + line.split(": ", 1)[1] + "\n")

    if not all_ok:
        print("\nFAILURE: one or more defects did not reproduce as predicted.", file=sys.stderr)
        sys.exit(1)
    print("\nAll six defects reproduced as predicted.")
    sys.exit(0)


if __name__ == "__main__":
    main()
