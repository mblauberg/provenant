#!/usr/bin/env python3
"""Focused text oracles for CAPA-001 normative spec repairs."""

import sqlite3
import unittest

from assert_fk import ForeignKeySpec, assert_fk_rejected
from spec_sources import (
    AGENT_FABRIC_BEHAVIOUR,
    AGENT_FABRIC_HARDENING,
    read_spec,
    read_specs,
)

BEHAVIOUR_SPECS = read_specs(AGENT_FABRIC_BEHAVIOUR)
HARDENING_SPECS = read_specs(AGENT_FABRIC_HARDENING)
PROVIDER_ACTIONS_SPEC = read_spec("agent-fabric/provider-actions-and-adapters.md")
MESSAGING_PROTOCOL_SPEC = read_spec("agent-fabric/messaging-and-public-protocol.md")


def ddl_block(text: str, table: str) -> str:
    start = text.index(f"\n{table}(") + 1
    end = text.index("\n)\n", start) + 3
    return text[start:end]


def trigger_sql(text: str, name: str) -> str:
    start = text.index(f"CREATE TRIGGER {name}\n")
    end = text.index("\nEND;", start) + len("\nEND;")
    return text[start:end]


TRIGGER_FIXTURE_SCHEMA = r"""
CREATE TABLE lifecycle_receipt_batch_completions(
  batch_id TEXT, transition_kind TEXT,
  primary_custody_effect_digest TEXT,
  primary_loss_effect_role TEXT, primary_loss_effect_digest TEXT,
  primary_retirement_effect_digest TEXT,
  linked_loss_effect_role TEXT, linked_loss_effect_digest TEXT,
  primary_fresh_effect_ordinal INTEGER, primary_fresh_effect_role TEXT,
  primary_fresh_effect_digest TEXT,
  secondary_fresh_effect_ordinal INTEGER, secondary_fresh_effect_role TEXT,
  secondary_fresh_effect_digest TEXT
);
CREATE TABLE lifecycle_receipt_custody_effects(
  batch_id TEXT, effect_digest TEXT, project_session_id TEXT, run_id TEXT,
  agent_id TEXT, custody_id TEXT, final_revision INTEGER,
  final_semantic_digest TEXT, final_source_ref_digest TEXT
);
CREATE TABLE lifecycle_receipt_generation_loss_effects(
  batch_id TEXT, role TEXT, effect_digest TEXT, project_session_id TEXT,
  run_id TEXT, agent_id TEXT, generation_loss_id TEXT,
  final_revision INTEGER, final_semantic_digest TEXT,
  final_source_ref_digest TEXT
);
CREATE TABLE lifecycle_receipt_recovery_retirement_effects(
  batch_id TEXT, effect_digest TEXT, retirement_id TEXT
);
CREATE TABLE lifecycle_receipt_fresh_origin_effects(
  batch_id TEXT, ordinal INTEGER, role TEXT, effect_digest TEXT
);
CREATE TABLE lifecycle_transition_applies(
  apply_id TEXT, apply_digest TEXT, apply_kind TEXT,
  batch_transition_kind TEXT, receipt_batch_id TEXT,
  fresh_generation_loss_after_key TEXT,
  fresh_project_session_id TEXT, fresh_run_id TEXT, fresh_agent_id TEXT,
  fresh_generation_loss_id TEXT, fresh_generation_loss_after_revision INTEGER,
  fresh_generation_loss_after_semantic_digest TEXT,
  fresh_generation_loss_after_source_ref_digest TEXT,
  fresh_handoff_id TEXT, fresh_source_mode TEXT, new_custody_id TEXT,
  new_custody_semantic_digest TEXT, new_custody_source_ref_digest TEXT
);
CREATE TABLE lifecycle_rotation_custody_revisions(
  project_session_id TEXT, run_id TEXT, agent_id TEXT, custody_id TEXT,
  revision INTEGER, semantic_digest TEXT, source_ref_digest TEXT,
  journal_digest TEXT, receipt_batch_id TEXT, receipt_apply_id TEXT,
  receipt_apply_digest TEXT, origin_fresh_apply_id TEXT,
  origin_fresh_apply_digest TEXT
);
CREATE TABLE lifecycle_rotation_custody_heads(
  project_session_id TEXT, run_id TEXT, agent_id TEXT, custody_id TEXT,
  current_revision INTEGER, semantic_digest TEXT, source_ref_digest TEXT,
  journal_digest TEXT
);
CREATE TABLE lifecycle_generation_loss_revisions(
  project_session_id TEXT, run_id TEXT, agent_id TEXT, generation_loss_id TEXT,
  revision INTEGER, semantic_digest TEXT, source_ref_digest TEXT,
  journal_digest TEXT, receipt_batch_id TEXT, receipt_apply_id TEXT,
  receipt_apply_digest TEXT, origin_fresh_apply_id TEXT,
  origin_fresh_apply_digest TEXT
);
CREATE TABLE lifecycle_generation_loss_heads(
  project_session_id TEXT, run_id TEXT, agent_id TEXT, generation_loss_id TEXT,
  current_revision INTEGER, semantic_digest TEXT, source_ref_digest TEXT,
  journal_digest TEXT
);
CREATE TABLE lifecycle_receipt_batches(
  batch_id TEXT, review_adoption_reservation_id TEXT,
  review_adoption_reservation_digest TEXT
);
CREATE TABLE lifecycle_review_authority_bindings(
  batch_id TEXT, apply_id TEXT, review_reservation_digest TEXT
);
CREATE TABLE agent_lifecycle_recovery_retirements(
  retirement_id TEXT, receipt_batch_id TEXT, receipt_apply_id TEXT,
  receipt_apply_digest TEXT, retirement_effect_digest TEXT
);
CREATE TABLE lifecycle_fresh_rotation_commits(
  handoff_id TEXT, apply_id TEXT, fresh_apply_digest TEXT,
  new_custody_id TEXT, generation_loss_after_id TEXT,
  generation_loss_after_revision INTEGER,
  generation_loss_after_semantic_digest TEXT,
  generation_loss_after_source_ref_digest TEXT
);
"""


def trigger_database() -> sqlite3.Connection:
    db = sqlite3.connect(":memory:")
    db.executescript(TRIGGER_FIXTURE_SCHEMA)
    for name in (
        "lifecycle_completion_effect_set_exact",
        "lifecycle_custody_effect_set_closed",
        "lifecycle_loss_effect_set_closed",
        "lifecycle_retirement_effect_set_closed",
        "lifecycle_fresh_origin_effect_set_closed",
        "lifecycle_apply_post_state_complete",
    ):
        db.executescript(trigger_sql(HARDENING_SPECS, name))
    return db


class SpecRepairTests(unittest.TestCase):
    def test_fresh_origin_effect_ddl_accepts_exact_and_rejects_crossed_arm(self) -> None:
        db = sqlite3.connect(":memory:", isolation_level=None)
        db.execute("PRAGMA foreign_keys=ON")
        db.executescript(
            """
            CREATE TABLE lifecycle_receipt_batches(
              batch_id TEXT, transition_kind TEXT, receipt_intent_count INTEGER,
              secondary_intent_kind TEXT, project_session_id TEXT,
              run_id TEXT, agent_id TEXT,
              UNIQUE(batch_id,transition_kind,receipt_intent_count),
              UNIQUE(batch_id,transition_kind,receipt_intent_count,
                secondary_intent_kind),
              UNIQUE(batch_id,project_session_id,run_id,agent_id)
            );
            CREATE TABLE lifecycle_fresh_recovery_handoffs(
              handoff_id TEXT, handoff_digest TEXT, planned_apply_id TEXT,
              project_session_id TEXT, run_id TEXT, agent_id TEXT,
              source_mode TEXT, recovery_source_kind TEXT,
              old_custody_id TEXT, old_custody_revision INTEGER,
              generation_loss_id TEXT, generation_loss_revision INTEGER,
              recovery_source_ref_digest TEXT, source_journal_digest TEXT,
              admission_digest TEXT, fresh_apply_plan_digest TEXT,
              new_custody_id TEXT, new_custody_semantic_digest TEXT,
              new_custody_source_ref_digest TEXT,
              affected_generation_loss_id TEXT,
              affected_generation_loss_before_revision INTEGER,
              affected_generation_loss_before_state TEXT,
              affected_generation_loss_before_source_ref_digest TEXT,
              affected_generation_loss_before_journal_digest TEXT,
              affected_generation_loss_after_revision INTEGER,
              affected_generation_loss_after_semantic_digest TEXT,
              affected_generation_loss_after_source_ref_digest TEXT,
              affected_generation_loss_after_key TEXT,
              UNIQUE(handoff_id,handoff_digest,planned_apply_id,
                project_session_id,run_id,agent_id,source_mode,
                recovery_source_kind,old_custody_id,old_custody_revision,
                generation_loss_id,generation_loss_revision,
                recovery_source_ref_digest,source_journal_digest,
                admission_digest,fresh_apply_plan_digest,new_custody_id,
                new_custody_semantic_digest,new_custody_source_ref_digest,
                affected_generation_loss_id,
                affected_generation_loss_before_revision,
                affected_generation_loss_before_source_ref_digest,
                affected_generation_loss_before_journal_digest,
                affected_generation_loss_after_revision,
                affected_generation_loss_after_semantic_digest,
                affected_generation_loss_after_source_ref_digest,
                affected_generation_loss_after_key),
              UNIQUE(handoff_id,handoff_digest,planned_apply_id,
                project_session_id,run_id,agent_id,source_mode,
                recovery_source_kind,recovery_source_ref_digest,
                source_journal_digest,new_custody_id,
                new_custody_semantic_digest,new_custody_source_ref_digest,
                affected_generation_loss_after_key,admission_digest,
                fresh_apply_plan_digest),
              UNIQUE(handoff_id,handoff_digest,affected_generation_loss_id,
                affected_generation_loss_before_revision,
                affected_generation_loss_before_state,
                affected_generation_loss_before_source_ref_digest,
                affected_generation_loss_before_journal_digest,
                affected_generation_loss_after_revision,
                affected_generation_loss_after_semantic_digest,
                affected_generation_loss_after_source_ref_digest)
            );
            CREATE TABLE lifecycle_receipt_custody_effects(
              batch_id TEXT,effect_digest TEXT,project_session_id TEXT,
              run_id TEXT,agent_id TEXT,custody_id TEXT,final_revision INTEGER,
              UNIQUE(batch_id,effect_digest,project_session_id,run_id,agent_id,
                custody_id,final_revision)
            );
            CREATE TABLE lifecycle_receipt_generation_loss_effects(
              batch_id TEXT,role TEXT,effect_digest TEXT,project_session_id TEXT,
              run_id TEXT,agent_id TEXT,generation_loss_id TEXT,
              final_revision INTEGER,
              UNIQUE(batch_id,role,effect_digest,project_session_id,run_id,
                agent_id,generation_loss_id,final_revision)
            );
            CREATE TABLE lifecycle_receipt_recovery_retirement_effects(
              batch_id TEXT,effect_digest TEXT,project_session_id TEXT,
              run_id TEXT,agent_id TEXT,retirement_id TEXT,
              retirement_revision INTEGER,
              UNIQUE(batch_id,effect_digest,project_session_id,run_id,agent_id,
                retirement_id,retirement_revision)
            );
            """
        )
        db.execute(
            "CREATE TABLE "
            + ddl_block(HARDENING_SPECS, "lifecycle_receipt_fresh_origin_effects")
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_batches VALUES(?,?,?,?,?,?,?)",
            ("batch", "fresh-origin", 1, "none", "session", "run", "agent"),
        )
        handoff = (
            "handoff", "handoff-digest", "apply", "session", "run", "agent",
            "reuse-final-custody", "custody", "old", 7, None, None,
            "source-ref", "source-journal", "admission", "plan", "new",
            "new-semantic", "new-source", None, None, None, None, None, None,
            None, None, "none",
        )
        db.execute(
            "INSERT INTO lifecycle_fresh_recovery_handoffs VALUES("
            + ",".join("?" for _ in handoff)
            + ")",
            handoff,
        )
        columns = (
            "batch_id,ordinal,role,transition_kind,batch_intent_count,"
            "batch_secondary_intent_kind,planned_apply_id,project_session_id,"
            "run_id,agent_id,handoff_id,handoff_digest,source_mode,"
            "recovery_source_kind,recovery_source_ref_digest,"
            "source_journal_digest,admission_digest,fresh_apply_plan_digest,"
            "new_custody_id,new_custody_revision,new_custody_semantic_digest,"
            "new_custody_source_ref_digest,affected_generation_loss_id,"
            "affected_generation_loss_before_revision,"
            "affected_generation_loss_before_state,"
            "affected_generation_loss_before_source_ref_digest,"
            "affected_generation_loss_before_journal_digest,"
            "affected_generation_loss_after_revision,"
            "affected_generation_loss_after_semantic_digest,"
            "affected_generation_loss_after_source_ref_digest,"
            "affected_generation_loss_after_key,effect_digest"
        )
        values = (
            "batch", 1, "primary", "fresh-origin", 1, "none", "apply",
            "session", "run", "agent", "handoff", "handoff-digest",
            "reuse-final-custody", "custody", "source-ref", "source-journal",
            "admission", "plan", "new", 1,
            "new-semantic", "new-source", None, None, None, None, None, None,
            None, None, "none", "effect",
        )
        statement = (
            f"INSERT INTO lifecycle_receipt_fresh_origin_effects({columns}) "
            f"VALUES({','.join('?' for _ in values)})"
        )
        db.execute(statement, values)
        db.execute(
            "CREATE TABLE " + ddl_block(HARDENING_SPECS, "lifecycle_receipt_intents")
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_intents("
            "batch_id,ordinal,batch_transition_kind,batch_intent_count,"
            "batch_secondary_intent_kind,kind,project_session_id,run_id,"
            "agent_id,subject_owner_kind,subject_owner_id,"
            "subject_owner_revision,fresh_origin_effect_digest,subject_json,"
            "subject_digest,intent_digest,created_at) "
            "VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            ("batch", 1, "fresh-origin", 1, "none", "fresh-origin", "session",
             "run", "agent", "custody", "new", 1, "effect", "{}", "subject",
             "intent", "created-at"),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_batches VALUES(?,?,?,?,?,?,?)",
            ("batch-terminal", "custody-terminal", 2, "fresh-origin",
             "session-terminal", "run-terminal", "agent-terminal"),
        )
        terminal_handoff = (
            "handoff-terminal", "handoff-digest-terminal", "apply-terminal",
            "session-terminal", "run-terminal", "agent-terminal",
            "terminalize-nonfinal-custody", "custody", "old-terminal", 1,
            None, None, "source-ref-terminal", "source-journal-terminal",
            "admission-terminal", "plan-terminal", "new-terminal",
            "new-semantic-terminal", "new-source-terminal", None, None, None,
            None, None, None, None, None, "none",
        )
        db.execute(
            "INSERT INTO lifecycle_fresh_recovery_handoffs VALUES("
            + ",".join("?" for _ in terminal_handoff)
            + ")",
            terminal_handoff,
        )
        terminal_values = (
            "batch-terminal", 2, "secondary", "custody-terminal", 2,
            "fresh-origin", "apply-terminal", "session-terminal",
            "run-terminal", "agent-terminal", "handoff-terminal",
            "handoff-digest-terminal", "terminalize-nonfinal-custody",
            "custody", "source-ref-terminal", "source-journal-terminal",
            "admission-terminal", "plan-terminal", "new-terminal", 1,
            "new-semantic-terminal", "new-source-terminal", None, None, None,
            None, None, None, None, None, "none", "effect-terminal",
        )
        db.execute(statement, terminal_values)
        db.execute(
            "INSERT INTO lifecycle_receipt_intents("
            "batch_id,ordinal,batch_transition_kind,batch_intent_count,"
            "batch_secondary_intent_kind,kind,project_session_id,run_id,"
            "agent_id,subject_owner_kind,subject_owner_id,"
            "subject_owner_revision,fresh_origin_effect_digest,subject_json,"
            "subject_digest,intent_digest,created_at) "
            "VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            ("batch-terminal", 2, "custody-terminal", 2, "fresh-origin",
             "fresh-origin", "session-terminal", "run-terminal",
             "agent-terminal", "custody", "new-terminal", 1,
             "effect-terminal", "{}", "subject-terminal", "intent-terminal",
             "created-at"),
        )
        additional_arms = (
            (
                ("batch-open", "fresh-origin", 1, "none", "session-open",
                 "run-open", "agent-open"),
                ("handoff-open", "handoff-digest-open", "apply-open",
                 "session-open", "run-open", "agent-open",
                 "open-generation-loss", "generation-loss", None, None,
                 "loss-open", 1, "loss-before-source-open",
                 "loss-before-journal-open", "admission-open", "plan-open",
                 "new-open", "new-semantic-open", "new-source-open",
                 "loss-open", 1, "open", "loss-before-source-open",
                 "loss-before-journal-open", 2, "loss-after-semantic-open",
                 "loss-after-source-open", "loss-after-source-open"),
                ("batch-open", 1, "primary", "fresh-origin", 1, "none",
                 "apply-open", "session-open", "run-open", "agent-open",
                 "handoff-open", "handoff-digest-open",
                 "open-generation-loss", "generation-loss",
                 "loss-before-source-open",
                 "loss-before-journal-open", "admission-open", "plan-open",
                 "new-open", 1, "new-semantic-open", "new-source-open",
                 "loss-open", 1, "open", "loss-before-source-open",
                 "loss-before-journal-open", 2, "loss-after-semantic-open",
                 "loss-after-source-open", "loss-after-source-open",
                 "effect-open"),
            ),
            (
                ("batch-terminal-linked", "custody-terminal", 2,
                 "fresh-origin", "session-terminal-linked",
                 "run-terminal-linked", "agent-terminal-linked"),
                ("handoff-terminal-linked", "handoff-digest-terminal-linked",
                 "apply-terminal-linked", "session-terminal-linked",
                 "run-terminal-linked", "agent-terminal-linked",
                 "terminalize-nonfinal-custody", "custody",
                 "old-terminal-linked", 1, None, None,
                 "source-ref-terminal-linked", "source-journal-terminal-linked",
                 "admission-terminal-linked", "plan-terminal-linked",
                 "new-terminal-linked", "new-semantic-terminal-linked",
                 "new-source-terminal-linked", "loss-terminal-linked", 1,
                 "open", "loss-before-source-terminal-linked",
                 "loss-before-journal-terminal-linked", 2,
                 "loss-after-semantic-terminal-linked",
                 "loss-after-source-terminal-linked",
                 "loss-after-source-terminal-linked"),
                ("batch-terminal-linked", 2, "secondary", "custody-terminal",
                 2, "fresh-origin", "apply-terminal-linked",
                 "session-terminal-linked", "run-terminal-linked",
                 "agent-terminal-linked", "handoff-terminal-linked",
                 "handoff-digest-terminal-linked",
                 "terminalize-nonfinal-custody", "custody",
                 "source-ref-terminal-linked", "source-journal-terminal-linked",
                 "admission-terminal-linked", "plan-terminal-linked",
                 "new-terminal-linked", 1, "new-semantic-terminal-linked",
                 "new-source-terminal-linked", "loss-terminal-linked", 1,
                 "open", "loss-before-source-terminal-linked",
                 "loss-before-journal-terminal-linked", 2,
                 "loss-after-semantic-terminal-linked",
                 "loss-after-source-terminal-linked",
                 "loss-after-source-terminal-linked", "effect-terminal-linked"),
            ),
        )
        for batch_row, handoff_row, effect_row in additional_arms:
            db.execute(
                "INSERT INTO lifecycle_receipt_batches VALUES(?,?,?,?,?,?,?)",
                batch_row,
            )
            db.execute(
                "INSERT INTO lifecycle_fresh_recovery_handoffs VALUES("
                + ",".join("?" for _ in handoff_row)
                + ")",
                handoff_row,
            )
            db.execute(statement, effect_row)
        db.execute(
            "DELETE FROM lifecycle_receipt_fresh_origin_effects "
            "WHERE batch_id='batch-open'"
        )
        crossed_open = list(additional_arms[0][2])
        crossed_open[16] = "crossed-admission"
        crossed_open[-1] = "effect-crossed-admission"
        assert_fk_rejected(
            db,
            invalid_operation=lambda connection: connection.execute(
                statement,
                crossed_open,
            ),
            positive_control=lambda connection: connection.execute(
                statement,
                additional_arms[0][2],
            ),
            expected=frozenset(
                {
                    ForeignKeySpec(
                        "lifecycle_receipt_fresh_origin_effects",
                        (
                            "handoff_id",
                            "handoff_digest",
                            "planned_apply_id",
                            "project_session_id",
                            "run_id",
                            "agent_id",
                            "source_mode",
                            "recovery_source_kind",
                            "recovery_source_ref_digest",
                            "source_journal_digest",
                            "new_custody_id",
                            "new_custody_semantic_digest",
                            "new_custody_source_ref_digest",
                            "affected_generation_loss_after_key",
                            "admission_digest",
                            "fresh_apply_plan_digest",
                        ),
                        "lifecycle_fresh_recovery_handoffs",
                        (
                            "handoff_id",
                            "handoff_digest",
                            "planned_apply_id",
                            "project_session_id",
                            "run_id",
                            "agent_id",
                            "source_mode",
                            "recovery_source_kind",
                            "recovery_source_ref_digest",
                            "source_journal_digest",
                            "new_custody_id",
                            "new_custody_semantic_digest",
                            "new_custody_source_ref_digest",
                            "affected_generation_loss_after_key",
                            "admission_digest",
                            "fresh_apply_plan_digest",
                        ),
                    )
                }
            ),
        )

        with self.assertRaises(sqlite3.IntegrityError) as caught:
            db.execute(
                "INSERT INTO lifecycle_receipt_intents("
                "batch_id,ordinal,batch_transition_kind,batch_intent_count,"
                "batch_secondary_intent_kind,kind,project_session_id,run_id,"
                "agent_id,subject_owner_kind,subject_owner_id,"
                "subject_owner_revision,fresh_origin_effect_digest,subject_json,"
                "subject_digest,intent_digest,created_at) "
                "VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                ("batch-terminal", 2, "custody-terminal", 2, "fresh-origin",
                 "fresh-origin", "session-terminal", "run-terminal",
                 "agent-terminal", "custody", "new-terminal", 1, "effect",
                 "{}", "subject-crossed", "intent-crossed", "created-at"),
            )
        self.assertEqual(
            str(caught.exception),
            "UNIQUE constraint failed: lifecycle_receipt_intents.kind, "
            "lifecycle_receipt_intents.project_session_id, "
            "lifecycle_receipt_intents.run_id, "
            "lifecycle_receipt_intents.agent_id, "
            "lifecycle_receipt_intents.subject_owner_kind, "
            "lifecycle_receipt_intents.subject_owner_id, "
            "lifecycle_receipt_intents.subject_owner_revision",
        )
        self.assertEqual([], db.execute("PRAGMA foreign_key_check").fetchall())

    def test_scope_admission_ddl_accepts_zero_member_and_rejects_near_valid(self) -> None:
        resolution_ddl = ddl_block(
            HARDENING_SPECS, "lifecycle_scope_admission_resolutions"
        )
        self.assertEqual(resolution_ddl.count("initial_head_receipt_digest"), 2)
        self.assertNotIn(
            "FOREIGN KEY(project_session_id,run_id,authority_id,"
            "initial_receipt_count",
            resolution_ddl,
        )
        self.assertNotIn(
            "FOREIGN KEY(project_id,namespace_checkpoint_digest,"
            "project_session_id,run_id",
            resolution_ddl,
        )
        db = sqlite3.connect(":memory:", isolation_level=None)
        db.execute("PRAGMA foreign_keys=ON")
        for table in (
            "lifecycle_scope_admission_outbox",
            "lifecycle_admitted_run_scopes",
            "lifecycle_receipt_scope_checkpoints",
            "lifecycle_receipt_namespace_checkpoints",
            "lifecycle_receipt_namespace_members",
            "lifecycle_scope_admission_resolutions",
            "lifecycle_receipt_scope_heads",
        ):
            db.execute("CREATE TABLE " + ddl_block(HARDENING_SPECS, table))
        for trigger in (
            "lifecycle_scope_admission_resolution_requires_complete_namespace",
            "lifecycle_scope_admission_outbox_no_update",
            "lifecycle_scope_admission_outbox_no_delete",
            "lifecycle_scope_admission_resolution_requires_initial_head",
            "lifecycle_scope_admission_resolution_no_update",
            "lifecycle_scope_admission_resolution_no_delete",
            "lifecycle_receipt_namespace_checkpoint_no_update",
            "lifecycle_receipt_namespace_checkpoint_no_delete",
            "lifecycle_receipt_namespace_member_no_update",
            "lifecycle_receipt_namespace_member_no_delete",
        ):
            db.executescript(trigger_sql(HARDENING_SPECS, trigger))

        db.execute(
            "INSERT INTO lifecycle_scope_admission_outbox("
            "admission_request_id,project_id,project_session_id,run_id,"
            "authority_id,admission_digest,admitted_at,scope_json,scope_digest,"
            "created_at) VALUES(?,?,?,?,?,?,?,?,?,?)",
            ("request", "project", "session", "run", "authority", "admission",
             "admitted-at", "{}", "scope", "created-at"),
        )
        db.execute("BEGIN")
        db.execute(
            "INSERT INTO lifecycle_admitted_run_scopes VALUES(?,?,?,?,?,?,?,?,?,?)",
            ("project", "session", "run", "authority", "request", "admission",
             "scope", "scope-checkpoint", "resolution", "admitted-at"),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_scope_checkpoints("
            "project_session_id,run_id,authority_id,receipt_count,"
            "head_authority_sequence,head_receipt_digest,ordered_record_set_digest,"
            "checkpoint_json,checkpoint_digest,attestation,verified_at) "
            "VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            ("session", "run", "authority", 0, 0, None, "empty-set", "{}",
             "scope-checkpoint", "attestation", "verified-at"),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_scope_heads VALUES(?,?,?,?)",
            ("session", "run", "scope-checkpoint", 1),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_namespace_checkpoints VALUES(?,?,?,?,?,?,?,?)",
            ("project", "authority", 1, "scope-head-set", "{}",
             "namespace-checkpoint", "attestation", "verified-at"),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_namespace_members VALUES(?,?,?,?,?,?,?,?,?)",
            ("project", "namespace-checkpoint", 1, "session", "run", "authority",
             "scope-checkpoint", 0, None),
        )
        db.execute(
            "INSERT INTO lifecycle_scope_admission_resolutions VALUES("
            + ",".join("?" for _ in range(13))
            + ")",
            ("request", "project", "session", "run", "authority", "scope",
             "scope-checkpoint", 0, None, "namespace-checkpoint", "{}",
             "resolution", "verified-at"),
        )
        db.commit()
        self.assertEqual([], db.execute("PRAGMA foreign_key_check").fetchall())

        with self.assertRaises(sqlite3.IntegrityError) as caught:
            db.execute(
                "INSERT INTO lifecycle_receipt_namespace_members VALUES("
                "?,?,?,?,?,?,?,?,?)",
                ("project", "namespace-checkpoint", 2, "other-session",
                 "other-run", "authority", "scope-checkpoint", 0,
                 "impossible-head"),
            )
        self.assertEqual(
            str(caught.exception),
            "CHECK constraint failed: "
            "(receipt_count=0)=(head_receipt_digest IS NULL)",
        )
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            db.execute(
                "UPDATE lifecycle_scope_admission_outbox SET created_at=created_at"
            )
        self.assertEqual(
            str(caught.exception),
            "lifecycle-scope-admission-outbox-immutable",
        )
        for statement in (
            "UPDATE lifecycle_scope_admission_resolutions "
            "SET verified_at=verified_at",
            "DELETE FROM lifecycle_scope_admission_resolutions",
        ):
            with self.subTest(resolution_mutation=statement.split()[0]):
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    db.execute(statement)
                self.assertEqual(
                    str(caught.exception),
                    "lifecycle-scope-admission-resolution-immutable",
                )
        for statement, marker in (
            (
                "UPDATE lifecycle_receipt_namespace_checkpoints "
                "SET verified_at=verified_at",
                "lifecycle-receipt-namespace-checkpoint-immutable",
            ),
            (
                "DELETE FROM lifecycle_receipt_namespace_checkpoints",
                "lifecycle-receipt-namespace-checkpoint-immutable",
            ),
            (
                "UPDATE lifecycle_receipt_namespace_members SET ordinal=ordinal",
                "lifecycle-receipt-namespace-member-immutable",
            ),
            (
                "DELETE FROM lifecycle_receipt_namespace_members",
                "lifecycle-receipt-namespace-member-immutable",
            ),
        ):
            with self.subTest(namespace_immutability=statement.split()[0:2]):
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    db.execute(statement)
                self.assertEqual(str(caught.exception), marker)

        db.execute(
            "INSERT INTO lifecycle_scope_admission_outbox("
            "admission_request_id,project_id,project_session_id,run_id,"
            "authority_id,admission_digest,admitted_at,scope_json,scope_digest,"
            "created_at) VALUES(?,?,?,?,?,?,?,?,?,?)",
            ("request-missing", "project-missing", "session-missing",
             "run-missing", "authority", "admission-missing", "admitted-at",
             "{}", "scope-missing", "created-at"),
        )
        db.execute("BEGIN")
        db.execute(
            "INSERT INTO lifecycle_admitted_run_scopes VALUES(?,?,?,?,?,?,?,?,?,?)",
            ("project-missing", "session-missing", "run-missing", "authority",
             "request-missing", "admission-missing", "scope-missing",
             "scope-checkpoint-missing", "resolution-missing", "admitted-at"),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_scope_checkpoints("
            "project_session_id,run_id,authority_id,receipt_count,"
            "head_authority_sequence,head_receipt_digest,ordered_record_set_digest,"
            "checkpoint_json,checkpoint_digest,attestation,verified_at) "
            "VALUES(?,?,?,?,?,?,?,?,?,?,?)",
            ("session-missing", "run-missing", "authority", 0, 0, None,
             "empty-set-missing", "{}", "scope-checkpoint-missing",
             "attestation", "verified-at"),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_namespace_checkpoints "
            "VALUES(?,?,?,?,?,?,?,?)",
            ("project-missing", "authority", 1, "scope-head-set-missing", "{}",
             "namespace-checkpoint-missing", "attestation", "verified-at"),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_namespace_members "
            "VALUES(?,?,?,?,?,?,?,?,?)",
            ("project-missing", "namespace-checkpoint-missing", 1,
             "session-missing", "run-missing", "authority",
             "scope-checkpoint-missing", 0, None),
        )
        db.execute(
            "INSERT INTO lifecycle_receipt_scope_heads VALUES(?,?,?,?)",
            ("session-missing", "run-missing", "scope-checkpoint-missing", 1),
        )
        resolution_insert = (
            "INSERT INTO lifecycle_scope_admission_resolutions VALUES("
            + ",".join("?" for _ in range(13))
            + ")"
        )
        resolution_values = (
            "request-missing", "project-missing", "session-missing",
            "run-missing", "authority", "scope-missing",
            "scope-checkpoint-missing", 0, None,
            "namespace-checkpoint-missing", "{}", "resolution-missing",
            "verified-at",
        )
        self.assertEqual(
            1,
            db.execute(
                "DELETE FROM lifecycle_receipt_scope_heads "
                "WHERE project_session_id='session-missing' "
                "AND run_id='run-missing'"
            ).rowcount,
        )
        with self.subTest(initial_head="crossed-only"):
            with self.assertRaises(sqlite3.IntegrityError) as caught:
                db.execute(resolution_insert, resolution_values)
            self.assertEqual(
                str(caught.exception),
                "lifecycle-scope-admission-initial-head-missing-or-crossed",
            )
        self.assertEqual(
            1,
            db.execute(
                "DELETE FROM lifecycle_receipt_scope_heads "
                "WHERE project_session_id='session' AND run_id='run'"
            ).rowcount,
        )
        with self.subTest(initial_head="missing"):
            with self.assertRaises(sqlite3.IntegrityError) as caught:
                db.execute(resolution_insert, resolution_values)
            self.assertEqual(
                str(caught.exception),
                "lifecycle-scope-admission-initial-head-missing-or-crossed",
            )
        db.rollback()

    def test_exact_batch_and_apply_ddl_accepts_only_complete_fresh_arms(self) -> None:
        db = sqlite3.connect(":memory:", isolation_level=None)
        db.execute("PRAGMA foreign_keys=ON")
        db.executescript(
            r"""
            CREATE TABLE lifecycle_review_adoption_reservations(
              reservation_id, reservation_digest, decision_loss_effect_key,
              decision_loss_after_id, decision_loss_after_revision,
              decision_loss_after_semantic_digest,
              decision_loss_after_source_ref_digest,
              UNIQUE(reservation_id,reservation_digest,decision_loss_effect_key),
              UNIQUE(reservation_id,reservation_digest,decision_loss_effect_key,
                decision_loss_after_id,decision_loss_after_revision,
                decision_loss_after_semantic_digest,
                decision_loss_after_source_ref_digest)
            );
            CREATE TABLE lifecycle_recovery_retirement_plans(
              retirement_id, retirement_plan_digest, planned_apply_id,
              project_session_id, run_id, agent_id, mutation_plan_digest,
              UNIQUE(retirement_id,retirement_plan_digest,planned_apply_id,
                project_session_id,run_id,agent_id,mutation_plan_digest)
            );
            CREATE TABLE lifecycle_receipt_generation_loss_effects(
              batch_id, role, effect_digest, project_session_id, run_id,
              agent_id, generation_loss_id, final_revision,
              final_semantic_digest, final_source_ref_digest,
              UNIQUE(batch_id,role,effect_digest,project_session_id,run_id,
                agent_id,generation_loss_id,final_revision,
                final_semantic_digest,final_source_ref_digest)
            );
            CREATE TABLE lifecycle_fresh_recovery_handoffs(
              handoff_id, handoff_digest, planned_apply_id, project_session_id,
              run_id, agent_id, source_mode, recovery_source_kind,
              old_custody_id, old_custody_revision, generation_loss_id,
              generation_loss_revision, recovery_source_ref_digest,
              source_journal_digest, admission_digest, fresh_apply_plan_digest,
              new_custody_id, new_custody_semantic_digest,
              new_custody_source_ref_digest, affected_generation_loss_id,
              affected_generation_loss_before_revision,
              affected_generation_loss_before_source_ref_digest,
              affected_generation_loss_before_journal_digest,
              affected_generation_loss_after_revision,
              affected_generation_loss_after_semantic_digest,
              affected_generation_loss_after_source_ref_digest,
              affected_generation_loss_after_key,
              UNIQUE(handoff_id,handoff_digest,planned_apply_id,source_mode),
              UNIQUE(handoff_id,handoff_digest,planned_apply_id,
                project_session_id,run_id,agent_id,source_mode,
                recovery_source_kind,old_custody_id,old_custody_revision,
                generation_loss_id,generation_loss_revision,
                recovery_source_ref_digest,source_journal_digest,
                admission_digest,fresh_apply_plan_digest,new_custody_id,
                new_custody_semantic_digest,new_custody_source_ref_digest,
                affected_generation_loss_id,
                affected_generation_loss_before_revision,
                affected_generation_loss_before_source_ref_digest,
                affected_generation_loss_before_journal_digest,
                affected_generation_loss_after_revision,
                affected_generation_loss_after_semantic_digest,
                affected_generation_loss_after_source_ref_digest,
                affected_generation_loss_after_key),
              UNIQUE(handoff_id,handoff_digest,planned_apply_id,
                project_session_id,run_id,agent_id,source_mode,new_custody_id,
                new_custody_semantic_digest,new_custody_source_ref_digest,
                fresh_apply_plan_digest,affected_generation_loss_after_key),
              UNIQUE(handoff_id,planned_apply_id,affected_generation_loss_id,
                affected_generation_loss_after_revision,
                affected_generation_loss_after_semantic_digest,
                affected_generation_loss_after_source_ref_digest)
            );
            CREATE TABLE lifecycle_receipt_batch_authorizations(
              batch_id, batch_completion_digest,
              ordered_authority_receipt_set_digest,
              verified_scope_checkpoint_digest,
              UNIQUE(batch_id,batch_completion_digest,
                ordered_authority_receipt_set_digest,
                verified_scope_checkpoint_digest)
            );
            """
        )
        db.execute("CREATE TABLE " + ddl_block(HARDENING_SPECS, "lifecycle_receipt_batches"))
        apply_ddl = ddl_block(HARDENING_SPECS, "lifecycle_transition_applies")
        unnamed_guard = "  CHECK((apply_kind='terminal' AND"
        self.assertEqual(apply_ddl.count(unnamed_guard), 1)
        apply_ddl = apply_ddl.replace(
            unnamed_guard,
            "  CONSTRAINT lifecycle_transition_applies_arm_guard "
            "CHECK((apply_kind='terminal' AND",
        )
        db.execute("CREATE TABLE " + apply_ddl)

        handoff_columns = (
            "handoff_id,handoff_digest,planned_apply_id,project_session_id,run_id,"
            "agent_id,source_mode,recovery_source_kind,old_custody_id,"
            "old_custody_revision,generation_loss_id,generation_loss_revision,"
            "recovery_source_ref_digest,source_journal_digest,admission_digest,"
            "fresh_apply_plan_digest,new_custody_id,new_custody_semantic_digest,"
            "new_custody_source_ref_digest,affected_generation_loss_id,"
            "affected_generation_loss_before_revision,"
            "affected_generation_loss_before_source_ref_digest,"
            "affected_generation_loss_before_journal_digest,"
            "affected_generation_loss_after_revision,"
            "affected_generation_loss_after_semantic_digest,"
            "affected_generation_loss_after_source_ref_digest,"
            "affected_generation_loss_after_key"
        )

        def seed_arm(
            suffix: str, mode: str, *, linked_loss: bool = False
        ) -> tuple[str, ...]:
            terminal = mode == "terminalize-nonfinal-custody"
            open_loss = mode == "open-generation-loss"
            affected = linked_loss or open_loss
            handoff_id = f"handoff-{suffix}"
            handoff_digest = f"handoff-digest-{suffix}"
            apply_id = f"apply-{suffix}"
            batch_id = f"batch-{suffix}"
            fresh_plan = f"fresh-plan-{suffix}"
            after_source = f"loss-after-source-{suffix}" if affected else None
            handoff = (
                handoff_id, handoff_digest, apply_id, f"session-{suffix}",
                f"run-{suffix}", f"agent-{suffix}", mode,
                "generation-loss" if open_loss else "custody",
                None if open_loss else f"old-custody-{suffix}",
                None if open_loss else 1,
                f"loss-{suffix}" if open_loss else None,
                1 if open_loss else None, f"source-ref-{suffix}",
                f"source-journal-{suffix}", f"admission-{suffix}", fresh_plan,
                f"new-custody-{suffix}", f"new-semantic-{suffix}",
                f"new-source-{suffix}", f"loss-{suffix}" if affected else None,
                1 if affected else None,
                f"loss-before-source-{suffix}" if affected else None,
                f"loss-before-journal-{suffix}" if affected else None,
                2 if affected else None, f"loss-after-semantic-{suffix}"
                if affected else None, after_source, after_source or "none",
            )
            db.execute(
                f"INSERT INTO lifecycle_fresh_recovery_handoffs({handoff_columns}) "
                f"VALUES({','.join('?' for _ in handoff)})",
                handoff,
            )
            transition = "custody-terminal" if terminal else "fresh-origin"
            apply_kind = "terminal-fresh" if terminal else "fresh"
            mutation_plan = f"terminal-plan-{suffix}" if terminal else fresh_plan
            db.execute(
                "INSERT INTO lifecycle_receipt_batches("
                "batch_id,planned_apply_id,project_session_id,run_id,agent_id,"
                "transition_kind,planned_apply_kind,effects_set_digest,"
                "mutation_plan_digest,transition_replay_json,"
                "transition_replay_digest,ordered_subject_set_digest,"
                "receipt_intent_count,secondary_intent_kind,"
                "review_decision_loss_effect_key,fresh_handoff_id,"
                "fresh_handoff_digest,fresh_handoff_source_mode,"
                "fresh_handoff_key,created_at) VALUES(?,?,?,?,?,?,?,?,?,?,?,"
                "?,?,?,?,?,?,?,?,?)",
                (batch_id, apply_id, f"session-{suffix}", f"run-{suffix}",
                 f"agent-{suffix}", transition, apply_kind,
                 f"effect-set-{suffix}", mutation_plan, "{}",
                 f"replay-{suffix}", f"subject-set-{suffix}",
                 2 if terminal else 1, "fresh-origin" if terminal else "none",
                 "none", handoff_id, handoff_digest, mode, handoff_digest,
                 "created-at"),
            )
            db.execute(
                "INSERT INTO lifecycle_receipt_batch_authorizations "
                "VALUES(?,?,?,?)",
                (batch_id, f"completion-{suffix}", f"receipt-set-{suffix}",
                 f"scope-checkpoint-{suffix}"),
            )
            return (
                apply_id, apply_kind, transition, batch_id,
                f"completion-{suffix}", f"replay-{suffix}",
                f"receipt-set-{suffix}", f"scope-checkpoint-{suffix}",
                mutation_plan, handoff_id, handoff_digest,
                handoff_digest,
                f"session-{suffix}", f"run-{suffix}", f"agent-{suffix}", mode,
                fresh_plan, f"new-custody-{suffix}", f"new-semantic-{suffix}",
                f"new-source-{suffix}", f"loss-{suffix}" if affected else None,
                2 if affected else None,
                f"loss-after-semantic-{suffix}" if affected else None,
                after_source, after_source or "none", f"write-set-{suffix}",
                "{}", f"apply-digest-{suffix}", "applied-at",
            )

        apply_columns = (
            "apply_id,apply_kind,batch_transition_kind,receipt_batch_id,"
            "batch_completion_digest,transition_replay_digest,"
            "ordered_authority_receipt_set_digest,verified_scope_checkpoint_digest,"
            "applied_mutation_plan_digest,fresh_handoff_id,fresh_handoff_digest,"
            "fresh_handoff_key,"
            "fresh_project_session_id,fresh_run_id,fresh_agent_id,"
            "fresh_source_mode,fresh_apply_plan_digest,new_custody_id,"
            "new_custody_semantic_digest,new_custody_source_ref_digest,"
            "fresh_generation_loss_id,fresh_generation_loss_after_revision,"
            "fresh_generation_loss_after_semantic_digest,"
            "fresh_generation_loss_after_source_ref_digest,"
            "fresh_generation_loss_after_key,local_write_set_digest,apply_json,"
            "apply_digest,applied_at"
        )
        valid = {}
        for suffix, mode, linked in (
            ("reuse", "reuse-final-custody", False),
            ("open", "open-generation-loss", False),
            ("terminal", "terminalize-nonfinal-custody", False),
            ("terminal-linked", "terminalize-nonfinal-custody", True),
        ):
            values = seed_arm(suffix, mode, linked_loss=linked)
            valid[suffix] = values
            db.execute(
                f"INSERT INTO lifecycle_transition_applies({apply_columns}) "
                f"VALUES({','.join('?' for _ in values)})",
                values,
            )

        self.assertEqual(
            [("terminal", None), ("terminal-linked", None)],
            db.execute(
                "SELECT substr(batch_id,7),review_adoption_reservation_id "
                "FROM lifecycle_receipt_batches "
                "WHERE planned_apply_kind='terminal-fresh' ORDER BY batch_id"
            ).fetchall(),
        )
        for suffix, mutation in (
            ("reuse", {3: None}),
            ("open", {2: "custody-terminal"}),
            ("terminal", {9: None, 10: None}),
            ("terminal-linked", {23: "crossed-loss-after-source"}),
        ):
            with self.subTest(near_valid_arm=suffix):
                values = list(valid[suffix])
                db.execute(
                    "DELETE FROM lifecycle_transition_applies WHERE apply_id=?",
                    (values[0],),
                )
                for index, value in mutation.items():
                    values[index] = value
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    db.execute(
                        f"INSERT INTO lifecycle_transition_applies({apply_columns}) "
                        f"VALUES({','.join('?' for _ in values)})",
                        values,
                    )
                self.assertEqual(
                    str(caught.exception),
                    "CHECK constraint failed: "
                    "lifecycle_transition_applies_arm_guard",
                )
                # Keep the specification-owned CHECK body, but assert the
                # shipped baseline's stable constraint identity.

        self.assertEqual([], db.execute("PRAGMA foreign_key_check").fetchall())

    def test_completion_triggers_accept_each_exact_effect_family(self) -> None:
        accepted = (
            (
                "custody",
                "INSERT INTO lifecycle_receipt_custody_effects "
                "(batch_id,effect_digest) VALUES "
                "('batch-custody','custody-effect')",
                "INSERT INTO lifecycle_receipt_batch_completions "
                "(batch_id,transition_kind,primary_custody_effect_digest) "
                "VALUES ('batch-custody','custody-terminal','custody-effect')",
            ),
            (
                "generation-loss",
                "INSERT INTO lifecycle_receipt_generation_loss_effects "
                "(batch_id,role,effect_digest) VALUES "
                "('batch-loss','primary','loss-effect')",
                "INSERT INTO lifecycle_receipt_batch_completions "
                "(batch_id,transition_kind,primary_loss_effect_role,"
                "primary_loss_effect_digest) VALUES "
                "('batch-loss','generation-loss-terminal','primary',"
                "'loss-effect')",
            ),
            (
                "retirement",
                "INSERT INTO lifecycle_receipt_recovery_retirement_effects "
                "(batch_id,effect_digest) VALUES "
                "('batch-retirement','retirement-effect')",
                "INSERT INTO lifecycle_receipt_batch_completions "
                "(batch_id,transition_kind,primary_retirement_effect_digest) "
                "VALUES ('batch-retirement','custody-recovery-retirement',"
                "'retirement-effect')",
            ),
            (
                "fresh-origin",
                "INSERT INTO lifecycle_receipt_fresh_origin_effects "
                "(batch_id,ordinal,role,effect_digest) VALUES "
                "('batch-fresh',1,'primary','fresh-effect')",
                "INSERT INTO lifecycle_receipt_batch_completions "
                "(batch_id,transition_kind,primary_fresh_effect_ordinal,"
                "primary_fresh_effect_role,primary_fresh_effect_digest) "
                "VALUES ('batch-fresh','fresh-origin',1,'primary',"
                "'fresh-effect')",
            ),
        )
        for family, effect, completion in accepted:
            with self.subTest(effect_family=family):
                db = trigger_database()
                db.execute(effect)
                db.execute(completion)
                self.assertEqual(
                    1,
                    db.execute(
                        "SELECT count(*) FROM lifecycle_receipt_batch_completions"
                    ).fetchone()[0],
                )

    def test_completion_triggers_reject_missing_extra_and_late_effects(self) -> None:
        missing_effects = (
            (
                "custody",
                "INSERT INTO lifecycle_receipt_batch_completions "
                "(batch_id,transition_kind,primary_custody_effect_digest) "
                "VALUES ('batch-missing','custody-terminal','custody-effect')",
            ),
            (
                "loss",
                "INSERT INTO lifecycle_receipt_batch_completions "
                "(batch_id,transition_kind,primary_loss_effect_role,"
                "primary_loss_effect_digest) VALUES "
                "('batch-missing','generation-loss-terminal','primary',"
                "'loss-effect')",
            ),
            (
                "retirement",
                "INSERT INTO lifecycle_receipt_batch_completions "
                "(batch_id,transition_kind,primary_retirement_effect_digest) "
                "VALUES ('batch-missing','custody-recovery-retirement',"
                "'retirement-effect')",
            ),
            (
                "fresh-origin",
                "INSERT INTO lifecycle_receipt_batch_completions "
                "(batch_id,transition_kind,primary_fresh_effect_ordinal,"
                "primary_fresh_effect_role,primary_fresh_effect_digest) "
                "VALUES ('batch-missing','fresh-origin',1,'primary',"
                "'fresh-effect')",
            ),
        )
        for name, statement in missing_effects:
            with self.subTest(missing_effect=name):
                missing = trigger_database()
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    missing.execute(statement)
                self.assertEqual(
                    str(caught.exception),
                    "lifecycle-effect-set-incomplete",
                )

        extra = trigger_database()
        extra.execute(
            "INSERT INTO lifecycle_receipt_custody_effects "
            "(batch_id,effect_digest) VALUES ('batch-extra','custody-effect')"
        )
        extra.execute(
            "INSERT INTO lifecycle_receipt_generation_loss_effects "
            "(batch_id,role,effect_digest) "
            "VALUES ('batch-extra','linked','extra-loss')"
        )
        with self.assertRaises(sqlite3.IntegrityError) as caught:
            extra.execute(
                "INSERT INTO lifecycle_receipt_batch_completions "
                "(batch_id,transition_kind,primary_custody_effect_digest) "
                "VALUES ('batch-extra','custody-terminal','custody-effect')"
            )
        self.assertEqual(
            str(caught.exception),
            "lifecycle-effect-set-incomplete",
        )

        late_inserts = (
            (
                "custody",
                "INSERT INTO lifecycle_receipt_custody_effects "
                "(batch_id,effect_digest) VALUES ('batch-closed','late')",
            ),
            (
                "loss",
                "INSERT INTO lifecycle_receipt_generation_loss_effects "
                "(batch_id,role,effect_digest) "
                "VALUES ('batch-closed','linked','late')",
            ),
            (
                "retirement",
                "INSERT INTO lifecycle_receipt_recovery_retirement_effects "
                "(batch_id,effect_digest) VALUES ('batch-closed','late')",
            ),
        )
        for name, statement in late_inserts:
            with self.subTest(effect_table=name):
                closed = trigger_database()
                closed.execute(
                    "INSERT INTO lifecycle_receipt_custody_effects "
                    "(batch_id,effect_digest) "
                    "VALUES ('batch-closed','custody-effect')"
                )
                closed.execute(
                    "INSERT INTO lifecycle_receipt_batch_completions "
                    "(batch_id,transition_kind,primary_custody_effect_digest) "
                    "VALUES ('batch-closed','custody-terminal','custody-effect')"
                )
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    closed.execute(statement)
                self.assertEqual(
                    str(caught.exception),
                    "lifecycle-effect-set-closed",
                )

    def _valid_apply_database(self) -> sqlite3.Connection:
        db = trigger_database()
        db.executescript(
            r"""
            INSERT INTO lifecycle_receipt_custody_effects
              (batch_id,effect_digest,project_session_id,run_id,agent_id,
               custody_id,final_revision,final_semantic_digest,
               final_source_ref_digest)
            VALUES
              ('batch-custody','effect-custody','p','r','a','custody-old',
               2,'sem-custody','src-custody');
            INSERT INTO lifecycle_receipt_batch_completions
              (batch_id,transition_kind,primary_custody_effect_digest)
            VALUES
              ('batch-custody','custody-terminal','effect-custody');
            INSERT INTO lifecycle_receipt_batches VALUES
              ('batch-custody',NULL,NULL);
            INSERT INTO lifecycle_rotation_custody_revisions VALUES
              ('p','r','a','custody-old',2,'sem-custody','src-custody',
               'journal-custody','batch-custody','apply-custody',
               'digest-custody',NULL,NULL);
            INSERT INTO lifecycle_rotation_custody_heads VALUES
              ('p','r','a','custody-old',2,'sem-custody','src-custody',
               'journal-custody');

            INSERT INTO lifecycle_receipt_generation_loss_effects
              (batch_id,role,effect_digest,project_session_id,run_id,agent_id,
               generation_loss_id,final_revision,final_semantic_digest,
               final_source_ref_digest)
            VALUES
              ('batch-loss','primary','effect-loss','p','r','a','loss-old',
               2,'sem-loss','src-loss');
            INSERT INTO lifecycle_receipt_batch_completions
              (batch_id,transition_kind,primary_loss_effect_role,
               primary_loss_effect_digest)
            VALUES
              ('batch-loss','generation-loss-terminal','primary','effect-loss');
            INSERT INTO lifecycle_generation_loss_revisions VALUES
              ('p','r','a','loss-old',2,'sem-loss','src-loss','journal-loss',
               'batch-loss','apply-loss','digest-loss',NULL,NULL);
            INSERT INTO lifecycle_generation_loss_heads VALUES
              ('p','r','a','loss-old',2,'sem-loss','src-loss','journal-loss');

            INSERT INTO lifecycle_receipt_recovery_retirement_effects
              (batch_id,effect_digest,retirement_id)
            VALUES
              ('batch-retirement','effect-retirement','retirement-1');
            INSERT INTO lifecycle_receipt_batch_completions
              (batch_id,transition_kind,primary_retirement_effect_digest)
            VALUES
              ('batch-retirement','custody-recovery-retirement',
               'effect-retirement');
            INSERT INTO agent_lifecycle_recovery_retirements VALUES
              ('retirement-1','batch-retirement','apply-retirement',
               'digest-retirement','effect-retirement');

            INSERT INTO lifecycle_receipt_custody_effects
              (batch_id,effect_digest,project_session_id,run_id,agent_id,
               custody_id,final_revision,final_semantic_digest,
               final_source_ref_digest)
            VALUES
              ('batch-terminal-fresh','effect-terminal-fresh','p','r','a',
               'custody-terminal-fresh-old',2,'sem-terminal-fresh-old',
               'src-terminal-fresh-old');
            INSERT INTO lifecycle_receipt_batch_completions
              (batch_id,transition_kind,primary_custody_effect_digest)
            VALUES
              ('batch-terminal-fresh','custody-terminal',
               'effect-terminal-fresh');
            INSERT INTO lifecycle_receipt_batches VALUES
              ('batch-terminal-fresh',NULL,NULL);
            INSERT INTO lifecycle_rotation_custody_revisions VALUES
              ('p','r','a','custody-terminal-fresh-old',2,
               'sem-terminal-fresh-old','src-terminal-fresh-old',
               'journal-terminal-fresh-old','batch-terminal-fresh',
               'apply-terminal-fresh','digest-terminal-fresh',NULL,NULL);
            INSERT INTO lifecycle_rotation_custody_heads VALUES
              ('p','r','a','custody-terminal-fresh-old',2,
               'sem-terminal-fresh-old','src-terminal-fresh-old',
               'journal-terminal-fresh-old');
            INSERT INTO lifecycle_rotation_custody_revisions VALUES
              ('p','r','a','custody-terminal-fresh-new',1,
               'sem-terminal-fresh-new','src-terminal-fresh-new',
               'journal-terminal-fresh-new',NULL,NULL,NULL,
               'apply-terminal-fresh','digest-terminal-fresh');
            INSERT INTO lifecycle_rotation_custody_heads VALUES
              ('p','r','a','custody-terminal-fresh-new',1,
               'sem-terminal-fresh-new','src-terminal-fresh-new',
               'journal-terminal-fresh-new');
            INSERT INTO lifecycle_fresh_rotation_commits VALUES
              ('handoff-terminal-fresh','apply-terminal-fresh',
               'digest-terminal-fresh','custody-terminal-fresh-new',
               NULL,NULL,NULL,NULL);

            INSERT INTO lifecycle_receipt_custody_effects
              (batch_id,effect_digest,project_session_id,run_id,agent_id,
               custody_id,final_revision,final_semantic_digest,
               final_source_ref_digest)
            VALUES
              ('batch-terminal-fresh-linked','effect-terminal-fresh-linked',
               'p','r','a','custody-terminal-fresh-linked-old',2,
               'sem-terminal-fresh-linked-old',
               'src-terminal-fresh-linked-old');
            INSERT INTO lifecycle_receipt_generation_loss_effects
              (batch_id,role,effect_digest,project_session_id,run_id,agent_id,
               generation_loss_id,final_revision,final_semantic_digest,
               final_source_ref_digest)
            VALUES
              ('batch-terminal-fresh-linked','linked',
               'effect-terminal-fresh-linked-loss','p','r','a',
               'loss-terminal-fresh-linked',2,
               'sem-terminal-fresh-linked-loss',
               'src-terminal-fresh-linked-loss');
            INSERT INTO lifecycle_receipt_batch_completions
              (batch_id,transition_kind,primary_custody_effect_digest,
               linked_loss_effect_role,linked_loss_effect_digest)
            VALUES
              ('batch-terminal-fresh-linked','custody-terminal',
               'effect-terminal-fresh-linked','linked',
               'effect-terminal-fresh-linked-loss');
            INSERT INTO lifecycle_receipt_batches VALUES
              ('batch-terminal-fresh-linked',NULL,NULL);
            INSERT INTO lifecycle_rotation_custody_revisions VALUES
              ('p','r','a','custody-terminal-fresh-linked-old',2,
               'sem-terminal-fresh-linked-old',
               'src-terminal-fresh-linked-old',
               'journal-terminal-fresh-linked-old',
               'batch-terminal-fresh-linked','apply-terminal-fresh-linked',
               'digest-terminal-fresh-linked',NULL,NULL);
            INSERT INTO lifecycle_rotation_custody_heads VALUES
              ('p','r','a','custody-terminal-fresh-linked-old',2,
               'sem-terminal-fresh-linked-old',
               'src-terminal-fresh-linked-old',
               'journal-terminal-fresh-linked-old');
            INSERT INTO lifecycle_rotation_custody_revisions VALUES
              ('p','r','a','custody-terminal-fresh-linked-new',1,
               'sem-terminal-fresh-linked-new',
               'src-terminal-fresh-linked-new',
               'journal-terminal-fresh-linked-new',NULL,NULL,NULL,
               'apply-terminal-fresh-linked','digest-terminal-fresh-linked');
            INSERT INTO lifecycle_rotation_custody_heads VALUES
              ('p','r','a','custody-terminal-fresh-linked-new',1,
               'sem-terminal-fresh-linked-new',
               'src-terminal-fresh-linked-new',
               'journal-terminal-fresh-linked-new');
            INSERT INTO lifecycle_generation_loss_revisions VALUES
              ('p','r','a','loss-terminal-fresh-linked',2,
               'sem-terminal-fresh-linked-loss',
               'src-terminal-fresh-linked-loss',
               'journal-terminal-fresh-linked-loss',
               'batch-terminal-fresh-linked','apply-terminal-fresh-linked',
               'digest-terminal-fresh-linked',NULL,NULL);
            INSERT INTO lifecycle_generation_loss_heads VALUES
              ('p','r','a','loss-terminal-fresh-linked',2,
               'sem-terminal-fresh-linked-loss',
               'src-terminal-fresh-linked-loss',
               'journal-terminal-fresh-linked-loss');
            INSERT INTO lifecycle_fresh_rotation_commits VALUES
              ('handoff-terminal-fresh-linked','apply-terminal-fresh-linked',
               'digest-terminal-fresh-linked',
               'custody-terminal-fresh-linked-new',
               'loss-terminal-fresh-linked',2,
               'sem-terminal-fresh-linked-loss',
               'src-terminal-fresh-linked-loss');

            INSERT INTO lifecycle_receipt_fresh_origin_effects VALUES
              ('batch-reuse',1,'primary','effect-reuse');
            INSERT INTO lifecycle_receipt_batch_completions
              (batch_id,transition_kind,primary_fresh_effect_ordinal,
               primary_fresh_effect_role,primary_fresh_effect_digest)
            VALUES ('batch-reuse','fresh-origin',1,'primary','effect-reuse');
            INSERT INTO lifecycle_receipt_batches VALUES
              ('batch-reuse',NULL,NULL);
            INSERT INTO lifecycle_rotation_custody_revisions VALUES
              ('p','r','a','custody-reuse',1,'sem-reuse','src-reuse',
               'journal-reuse',NULL,NULL,NULL,'apply-reuse','digest-reuse');
            INSERT INTO lifecycle_rotation_custody_heads VALUES
              ('p','r','a','custody-reuse',1,'sem-reuse','src-reuse',
               'journal-reuse');
            INSERT INTO lifecycle_fresh_rotation_commits VALUES
              ('handoff-reuse','apply-reuse','digest-reuse','custody-reuse',
               NULL,NULL,NULL,NULL);

            INSERT INTO lifecycle_receipt_fresh_origin_effects VALUES
              ('batch-open',1,'primary','effect-open');
            INSERT INTO lifecycle_receipt_batch_completions
              (batch_id,transition_kind,primary_fresh_effect_ordinal,
               primary_fresh_effect_role,primary_fresh_effect_digest)
            VALUES ('batch-open','fresh-origin',1,'primary','effect-open');
            INSERT INTO lifecycle_receipt_batches VALUES
              ('batch-open',NULL,NULL);
            INSERT INTO lifecycle_rotation_custody_revisions VALUES
              ('p','r','a','custody-open',1,'sem-open','src-open',
               'journal-open',NULL,NULL,NULL,'apply-open','digest-open');
            INSERT INTO lifecycle_rotation_custody_heads VALUES
              ('p','r','a','custody-open',1,'sem-open','src-open',
               'journal-open');
            INSERT INTO lifecycle_generation_loss_revisions VALUES
              ('p','r','a','loss-open',2,'sem-loss-open','src-loss-open',
               'journal-loss-open',NULL,NULL,NULL,'apply-open','digest-open');
            INSERT INTO lifecycle_generation_loss_heads VALUES
              ('p','r','a','loss-open',2,'sem-loss-open','src-loss-open',
               'journal-loss-open');
            INSERT INTO lifecycle_fresh_rotation_commits VALUES
              ('handoff-open','apply-open','digest-open','custody-open',
               'loss-open',2,'sem-loss-open','src-loss-open');
            """
        )
        return db

    APPLY_STATEMENTS = (
            (
                "terminal-custody",
                "INSERT INTO lifecycle_transition_applies "
                "(apply_id,apply_digest,apply_kind,batch_transition_kind,"
                "receipt_batch_id,fresh_generation_loss_after_key) "
                "VALUES ('apply-custody','digest-custody','terminal',"
                "'custody-terminal','batch-custody','none')",
            ),
            (
                "terminal-generation-loss",
                "INSERT INTO lifecycle_transition_applies "
                "(apply_id,apply_digest,apply_kind,batch_transition_kind,"
                "receipt_batch_id,fresh_generation_loss_after_key) "
                "VALUES ('apply-loss','digest-loss','terminal',"
                "'generation-loss-terminal','batch-loss','none')",
            ),
            (
                "terminal-retirement",
                "INSERT INTO lifecycle_transition_applies "
                "(apply_id,apply_digest,apply_kind,batch_transition_kind,"
                "receipt_batch_id,fresh_generation_loss_after_key) "
                "VALUES ('apply-retirement','digest-retirement','terminal',"
                "'custody-recovery-retirement','batch-retirement','none')",
            ),
            (
                "terminal-fresh",
                "INSERT INTO lifecycle_transition_applies "
                "(apply_id,apply_digest,apply_kind,batch_transition_kind,"
                "receipt_batch_id,fresh_generation_loss_after_key,"
                "fresh_project_session_id,fresh_run_id,fresh_agent_id,"
                "fresh_handoff_id,fresh_source_mode,new_custody_id,"
                "new_custody_semantic_digest,new_custody_source_ref_digest) "
                "VALUES ('apply-terminal-fresh','digest-terminal-fresh',"
                "'terminal-fresh','custody-terminal','batch-terminal-fresh',"
                "'none','p','r','a','handoff-terminal-fresh',"
                "'terminalize-nonfinal-custody',"
                "'custody-terminal-fresh-new','sem-terminal-fresh-new',"
                "'src-terminal-fresh-new')",
            ),
            (
                "terminal-fresh-linked-loss",
                "INSERT INTO lifecycle_transition_applies "
                "(apply_id,apply_digest,apply_kind,batch_transition_kind,"
                "receipt_batch_id,fresh_generation_loss_after_key,"
                "fresh_project_session_id,fresh_run_id,fresh_agent_id,"
                "fresh_generation_loss_id,fresh_generation_loss_after_revision,"
                "fresh_generation_loss_after_semantic_digest,"
                "fresh_generation_loss_after_source_ref_digest,"
                "fresh_handoff_id,fresh_source_mode,new_custody_id,"
                "new_custody_semantic_digest,new_custody_source_ref_digest) "
                "VALUES ('apply-terminal-fresh-linked',"
                "'digest-terminal-fresh-linked','terminal-fresh',"
                "'custody-terminal','batch-terminal-fresh-linked',"
                "'src-terminal-fresh-linked-loss','p','r','a',"
                "'loss-terminal-fresh-linked',2,"
                "'sem-terminal-fresh-linked-loss',"
                "'src-terminal-fresh-linked-loss',"
                "'handoff-terminal-fresh-linked',"
                "'terminalize-nonfinal-custody',"
                "'custody-terminal-fresh-linked-new',"
                "'sem-terminal-fresh-linked-new',"
                "'src-terminal-fresh-linked-new')",
            ),
            (
                "fresh-reuse",
                "INSERT INTO lifecycle_transition_applies "
                "(apply_id,apply_digest,apply_kind,batch_transition_kind,"
                "receipt_batch_id,fresh_generation_loss_after_key,"
                "fresh_project_session_id,"
                "fresh_run_id,fresh_agent_id,fresh_handoff_id,"
                "fresh_source_mode,new_custody_id,new_custody_semantic_digest,"
                "new_custody_source_ref_digest) VALUES "
                "('apply-reuse','digest-reuse','fresh','fresh-origin',"
                "'batch-reuse','none','p','r',"
                "'a','handoff-reuse','reuse-final-custody','custody-reuse',"
                "'sem-reuse','src-reuse')",
            ),
            (
                "fresh-open-generation-loss",
                "INSERT INTO lifecycle_transition_applies "
                "(apply_id,apply_digest,apply_kind,batch_transition_kind,"
                "receipt_batch_id,fresh_generation_loss_after_key,"
                "fresh_project_session_id,"
                "fresh_run_id,fresh_agent_id,fresh_generation_loss_id,"
                "fresh_generation_loss_after_revision,"
                "fresh_generation_loss_after_semantic_digest,"
                "fresh_generation_loss_after_source_ref_digest,"
                "fresh_handoff_id,fresh_source_mode,new_custody_id,"
                "new_custody_semantic_digest,new_custody_source_ref_digest) "
                "VALUES ('apply-open','digest-open','fresh','fresh-origin',"
                "'batch-open','src-loss-open','p','r','a','loss-open',2,"
                "'sem-loss-open',"
                "'src-loss-open','handoff-open','open-generation-loss',"
                "'custody-open','sem-open','src-open')",
            ),
        )

    def test_apply_post_state_trigger_accepts_all_seven_materialized_branches(self) -> None:
        db = self._valid_apply_database()
        for arm, statement in self.APPLY_STATEMENTS:
            with self.subTest(apply_arm=arm):
                db.execute(statement)

        self.assertEqual(
            7,
            db.execute(
                "SELECT count(*) FROM lifecycle_transition_applies"
            ).fetchone()[0],
        )

    def test_apply_marker_requires_complete_arm_specific_post_state(self) -> None:
        apply_trigger = trigger_sql(
            HARDENING_SPECS, "lifecycle_apply_post_state_complete"
        )
        self.assertIn("lifecycle-apply-post-state-incomplete", apply_trigger)
        self.assertIn("NEW.batch_transition_kind='custody-terminal'", apply_trigger)
        self.assertIn("NEW.apply_kind='terminal-fresh'", apply_trigger)
        self.assertIn("NEW.fresh_source_mode='reuse-final-custody'", apply_trigger)
        self.assertIn("NEW.fresh_source_mode='open-generation-loss'", apply_trigger)

        broken_post_states = (
            (
                "terminal-custody",
                "DELETE FROM lifecycle_rotation_custody_heads "
                "WHERE custody_id='custody-old'",
            ),
            (
                "terminal-generation-loss",
                "DELETE FROM lifecycle_generation_loss_heads "
                "WHERE generation_loss_id='loss-old'",
            ),
            (
                "terminal-retirement",
                "DELETE FROM agent_lifecycle_recovery_retirements "
                "WHERE retirement_id='retirement-1'",
            ),
            (
                "terminal-fresh",
                "DELETE FROM lifecycle_fresh_rotation_commits "
                "WHERE apply_id='apply-terminal-fresh'",
            ),
            (
                "terminal-fresh-linked-loss",
                "DELETE FROM lifecycle_generation_loss_heads "
                "WHERE generation_loss_id='loss-terminal-fresh-linked'",
            ),
            (
                "fresh-reuse",
                "DELETE FROM lifecycle_fresh_rotation_commits "
                "WHERE apply_id='apply-reuse'",
            ),
            (
                "fresh-open-generation-loss",
                "DELETE FROM lifecycle_generation_loss_heads "
                "WHERE generation_loss_id='loss-open'",
            ),
        )
        apply_statements = dict(self.APPLY_STATEMENTS)
        for arm, break_post_state in broken_post_states:
            with self.subTest(apply_arm=arm):
                db = self._valid_apply_database()
                self.assertEqual(1, db.execute(break_post_state).rowcount)
                with self.assertRaises(sqlite3.IntegrityError) as caught:
                    db.execute(apply_statements[arm])
                self.assertEqual(
                    str(caught.exception),
                    "lifecycle-apply-post-state-incomplete",
                )

    def test_normative_lifecycle_head_ddl_rejects_null_vacuity(self) -> None:
        db = sqlite3.connect(":memory:")
        db.execute("PRAGMA foreign_keys=ON")
        db.executescript("""
            CREATE TABLE lifecycle_rotation_custody_revisions(
              project_session_id,run_id,agent_id,custody_id,revision,state,
              disposition_code,semantic_digest,source_ref_digest,journal_digest,
              UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
                state,disposition_code,semantic_digest,source_ref_digest,
                journal_digest));
            CREATE TABLE lifecycle_generation_loss_revisions(
              project_session_id,run_id,agent_id,generation_loss_id,revision,
              state,abandon_kind_code,semantic_digest,source_ref_digest,
              journal_digest,
              UNIQUE(project_session_id,run_id,agent_id,generation_loss_id,
                revision,state,abandon_kind_code,semantic_digest,
                source_ref_digest,journal_digest));
        """)
        for table in (
            "lifecycle_rotation_custody_heads",
            "lifecycle_generation_loss_heads",
        ):
            db.execute("CREATE TABLE " + ddl_block(HARDENING_SPECS, table))
        db.execute(
            "INSERT INTO lifecycle_rotation_custody_revisions "
            "VALUES(?,?,?,?,?,?,?,?,?,?)",
            ("session", "run", "agent", "custody", 1, "finalized",
             "adopted", "semantic", "source", "journal"),
        )
        db.execute(
            "INSERT INTO lifecycle_generation_loss_revisions "
            "VALUES(?,?,?,?,?,?,?,?,?,?)",
            ("session", "run", "agent", "loss", 1, "abandoned",
             "direct-open", "semantic", "source", "journal"),
        )
        db.commit()

        for label, statement, values, message in (
            (
                "custody-null-revision",
                "INSERT INTO lifecycle_rotation_custody_heads "
                "VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                ("session", "run", "agent", "custody", None, "finalized",
                 "adopted", "semantic", "source", "journal", 1, 1),
                "NOT NULL constraint failed: "
                "lifecycle_rotation_custody_heads.current_revision",
            ),
            (
                "custody-null-terminal",
                "INSERT INTO lifecycle_rotation_custody_heads "
                "VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                ("session", "run", "agent", "custody", 1, "finalized",
                 "adopted", "semantic", "source", "journal", None, 1),
                "NOT NULL constraint failed: "
                "lifecycle_rotation_custody_heads.terminal",
            ),
            (
                "loss-null-revision",
                "INSERT INTO lifecycle_generation_loss_heads "
                "VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                ("session", "run", "agent", "loss", None, "abandoned",
                 "direct-open", "semantic", "source", "journal", 1, 1),
                "NOT NULL constraint failed: "
                "lifecycle_generation_loss_heads.current_revision",
            ),
            (
                "loss-null-terminal",
                "INSERT INTO lifecycle_generation_loss_heads "
                "VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                ("session", "run", "agent", "loss", 1, "abandoned",
                 "direct-open", "semantic", "source", "journal", None, 1),
                "NOT NULL constraint failed: "
                "lifecycle_generation_loss_heads.terminal",
            ),
            (
                "custody-missing-parent",
                "INSERT INTO lifecycle_rotation_custody_heads "
                "VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                ("session", "run", "agent", "missing-custody", 1, "finalized",
                 "adopted", "semantic", "source", "journal", 1, 1),
                "structural-foreign-key",
            ),
            (
                "loss-missing-parent",
                "INSERT INTO lifecycle_generation_loss_heads "
                "VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",
                ("session", "run", "agent", "missing-loss", 1, "abandoned",
                 "direct-open", "semantic", "source", "journal", 1, 1),
                "structural-foreign-key",
            ),
        ):
            with self.subTest(label=label):
                if message != "structural-foreign-key":
                    with self.assertRaises(sqlite3.IntegrityError) as caught:
                        db.execute(statement, values)
                    self.assertEqual(str(caught.exception), message)
                    db.rollback()
                    continue
                custody = label.startswith("custody")
                positive_values = (
                    (
                        "session", "run", "agent", "custody", 1, "finalized",
                        "adopted", "semantic", "source", "journal", 1, 1,
                    )
                    if custody
                    else (
                        "session", "run", "agent", "loss", 1, "abandoned",
                        "direct-open", "semantic", "source", "journal", 1, 1,
                    )
                )
                child_table = (
                    "lifecycle_rotation_custody_heads"
                    if custody
                    else "lifecycle_generation_loss_heads"
                )
                parent_table = (
                    "lifecycle_rotation_custody_revisions"
                    if custody
                    else "lifecycle_generation_loss_revisions"
                )
                owner_column = "custody_id" if custody else "generation_loss_id"
                state_column = "state"
                disposition_column = (
                    "disposition_code" if custody else "abandon_kind_code"
                )
                assert_fk_rejected(
                    db,
                    invalid_operation=lambda connection, statement=statement,
                    values=values: connection.execute(statement, values),
                    positive_control=lambda connection, statement=statement,
                    values=positive_values: connection.execute(statement, values),
                    expected=frozenset(
                        {
                            ForeignKeySpec(
                                child_table,
                                (
                                    "project_session_id",
                                    "run_id",
                                    "agent_id",
                                    owner_column,
                                    "current_revision",
                                    state_column,
                                    disposition_column,
                                    "semantic_digest",
                                    "source_ref_digest",
                                    "journal_digest",
                                ),
                                parent_table,
                                (
                                    "project_session_id",
                                    "run_id",
                                    "agent_id",
                                    owner_column,
                                    "revision",
                                    state_column,
                                    disposition_column,
                                    "semantic_digest",
                                    "source_ref_digest",
                                    "journal_digest",
                                ),
                            )
                        }
                    ),
                )
        self.assertEqual([], db.execute("PRAGMA foreign_key_check").fetchall())

    def test_normative_review_evidence_ddl_and_missing_target_parent(self) -> None:
        db = sqlite3.connect(":memory:")
        db.execute("PRAGMA foreign_keys=ON")
        db.executescript("""
            CREATE TABLE provider_actions(
              adapter_id,action_id,PRIMARY KEY(adapter_id,action_id));
            CREATE TABLE provider_action_routes(
              adapter_id,action_id,route_receipt_digest,
              deployed_route_admission_digest,run_id,target_generation,slot,
              attempt_generation,
              PRIMARY KEY(adapter_id,action_id),
              UNIQUE(adapter_id,action_id,deployed_route_admission_digest),
              UNIQUE(adapter_id,action_id,run_id,target_generation,slot,
                attempt_generation));
            CREATE TABLE review_finding_capacity_reservations(
              adapter_id,action_id,run_id,target_generation,slot,
              attempt_generation,reservation_digest,
              UNIQUE(adapter_id,action_id,run_id,target_generation,slot,
                attempt_generation,reservation_digest));
            CREATE TABLE review_completion_targets(
              run_id,target_generation,task_id,bundle_digest,coverage_digest,
              resolved_profile_digest,
              UNIQUE(run_id,target_generation,task_id,bundle_digest,
                coverage_digest,resolved_profile_digest));
            CREATE TABLE review_target_chair_bindings(
              run_id,target_generation,binding_generation,binding_digest,
              task_id,bundle_digest,profile_digest,
              UNIQUE(run_id,target_generation,binding_generation,
                binding_digest,task_id,bundle_digest,profile_digest));
            CREATE TABLE review_finding_sets(
              finding_set_digest PRIMARY KEY);
        """)
        db.execute(
            "CREATE TABLE "
            + ddl_block(HARDENING_SPECS, "provider_action_route_observations")
        )
        for table in (
            "provider_review_terminal_journal",
            "provider_review_results",
            "provider_review_evidence",
            "review_slot_heads",
        ):
            db.execute("CREATE TABLE " + ddl_block(HARDENING_SPECS, table))
        db.execute("INSERT INTO provider_actions VALUES('adapter','action')")
        db.execute(
            "INSERT INTO provider_action_routes VALUES(?,?,?,?,?,?,?,?)",
            ("adapter", "action", "route-receipt", "admission", "run", 1,
             "native", 1),
        )
        db.execute(
            "INSERT INTO provider_action_route_observations VALUES(?,?,?,?,?,?)",
            ("adapter", "action", "admission", "{}", "observation",
             "observed-at"),
        )
        db.execute(
            "INSERT INTO provider_review_terminal_journal "
            "VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            ("adapter", "action", "run", 1, "native", 1,
             "unusable-answer", 1, "terminal-input", "answer", None,
             "adapter-result", "usage", "read-journal", "projection", None,
             1),
        )
        db.execute(
            "INSERT INTO provider_review_results VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
            ("adapter", "action", 1, "unusable-answer", "answer", 0,
             None, "result", None, None, "classifier", "selector", None,
             None, 1),
        )
        db.execute("INSERT INTO review_finding_sets VALUES('empty-set')")
        db.execute(
            "INSERT INTO review_finding_capacity_reservations "
            "VALUES(?,?,?,?,?,?,?)",
            ("adapter", "action", "run", 1, "native", 1, "reservation"),
        )
        db.execute(
            "INSERT INTO review_completion_targets VALUES(?,?,?,?,?,?)",
            ("run", 1, "task", "bundle", "coverage", "profile"),
        )
        db.execute(
            "INSERT INTO review_target_chair_bindings VALUES(?,?,?,?,?,?,?)",
            ("run", 1, 1, "chair-binding", "task", "bundle", "profile"),
        )

        evidence = {
            "run_id": "run",
            "evidence_id": "evidence",
            "target_generation": 1,
            "slot": "native",
            "task_id": "task",
            "action_adapter_id": "adapter",
            "action_id": "action",
            "terminal_sequence": 1,
            "terminal_kind": "unusable-answer",
            "verdict": "UNUSABLE",
            "answer_safety": "unusable",
            "provider_answer_digest": "answer",
            "terminal_result_digest": "result",
            "review_result_digest": None,
            "route_receipt_digest": "route-receipt",
            "route_admission_digest": "admission",
            "route_observation_digest": "observation",
            "actual_route_identity_digest": "actual-route",
            "final_prompt_digest": "prompt",
            "endpoint_provider": "provider",
            "provider_family": "family",
            "model": "model",
            "bundle_digest": "bundle",
            "coverage_digest": "coverage",
            "profile_digest": "profile",
            "chair_binding_generation": 1,
            "chair_binding_digest": "chair-binding",
            "prior_head_generation": 0,
            "new_head_generation": 1,
            "attempt_generation": 1,
            "prior_evidence_id": None,
            "prior_open_finding_set_digest": "empty-set",
            "reported_resolved_finding_set_digest": "empty-set",
            "accepted_resolved_finding_set_digest": "empty-set",
            "finding_set_digest": "empty-set",
            "new_open_finding_set_digest": "empty-set",
            "repair_required_finding_set_digest": "empty-set",
            "finding_window_digest": "finding-window",
            "finding_capacity_reservation_digest": "reservation",
            "read_coverage_digest": "read-coverage",
            "coverage_summary_digest": "coverage-summary",
            "reviewer_family_relation": "family-unproved",
            "certification_basis_at_terminal_digest": "certification-basis",
            "mutation_receipt_digest": "mutation-receipt",
            "evidence_json": "{}",
            "evidence_digest": "evidence-digest",
            "created_at": 3,
        }

        def insert_evidence(row: dict[str, object]) -> None:
            columns = ",".join(row)
            placeholders = ",".join("?" for _ in row)
            db.execute(
                f"INSERT INTO provider_review_evidence({columns}) "
                f"VALUES({placeholders})",
                tuple(row.values()),
            )

        insert_evidence(evidence)
        db.commit()
        assert_fk_rejected(
            db,
            invalid_operation=lambda connection: connection.execute(
                "INSERT INTO review_slot_heads VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                ("run", 2, "native", 1, "fabricated-evidence", 0,
                 None, None, None, "empty-set", "empty-set", None, None,
                 1, "updated-at"),
            ),
            positive_control=lambda connection: connection.execute(
                "INSERT INTO review_slot_heads VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                ("run", 1, "native", 1, "evidence", 0,
                 None, None, None, "empty-set", "empty-set", None, None,
                 1, "updated-at"),
            ),
            expected=frozenset(
                {
                    ForeignKeySpec(
                        "review_slot_heads",
                        (
                            "run_id",
                            "target_generation",
                            "slot",
                            "head_generation",
                            "head_evidence_id",
                        ),
                        "provider_review_evidence",
                        (
                            "run_id",
                            "target_generation",
                            "slot",
                            "new_head_generation",
                            "evidence_id",
                        ),
                    )
                }
            ),
        )
        self.assertEqual([], db.execute("PRAGMA foreign_key_check").fetchall())

    def test_new_route_sections_have_unique_requirement_anchors(self) -> None:
        section_21_start = PROVIDER_ACTIONS_SPEC.index(
            "### Capability-backed deployed routes and operational telemetry"
        )
        section_22_start = MESSAGING_PROTOCOL_SPEC.index(
            "### Exact Console read identity completion"
        )
        sections = {
            PROVIDER_ACTIONS_SPEC[section_21_start:]: [
                *(f"FR-{number:03d}" for number in range(77, 89)),
                *(f"NFR-{number:03d}" for number in range(34, 39)),
                *(f"AC-{number:03d}" for number in range(56, 64)),
            ],
            MESSAGING_PROTOCOL_SPEC[section_22_start:]: [
                *(f"FR-{number:03d}" for number in range(89, 96)),
                *(f"NFR-{number:03d}" for number in range(39, 43)),
                *(f"AC-{number:03d}" for number in range(64, 71)),
            ],
        }
        for section, expected in sections.items():
            self.assertEqual(section.count("Added requirements are:"), 1)
            self.assertEqual(
                section.count("Acceptance additionally requires:"), 1
            )
            for requirement_id in expected:
                with self.subTest(requirement_id=requirement_id):
                    self.assertEqual(section.count(f"**{requirement_id}:**"), 1)
        all_expected = [
            item for expected in sections.values() for item in expected
        ]
        for requirement_id in all_expected:
            with self.subTest(requirement_id=requirement_id):
                self.assertEqual(BEHAVIOUR_SPECS.count(f"**{requirement_id}:**"), 1)

    def test_lifecycle_mutation_plan_binds_provider_action_update(self) -> None:
        start = BEHAVIOUR_SPECS.index("`lifecycleMutationPlanV1`")
        end = BEHAVIOUR_SPECS.index("An owner-transition receipt effect", start)
        section = " ".join(BEHAVIOUR_SPECS[start:end].split())
        enum_start = section.index("The closed relation enum is:")
        enum_end = section.index("`writeSetDigest=", enum_start)
        relation_enum = section[enum_start:enum_end]
        self.assertEqual(relation_enum.count("`provider-action`"), 1)
        self.assertIn(
            "The `provider-action` member is update-only. Its `keyDigest` "
            "binds the exact daemon-global `ProviderActionRefV1` pair "
            "`{adapterId,actionId}`",
            section,
        )
        self.assertIn(
            "a normal `mutationPlan` equality-copies the replay's non-null "
            "`providerActionRef`",
            section,
        )
        self.assertIn(
            "a `freshApplyPlan` equality-copies its enclosing "
            "`freshRecoveryHandoffV1.replacementActionRef`",
            section,
        )
        self.assertIn("insert, delete or any different pair is invalid", section)


if __name__ == "__main__":
    unittest.main()
