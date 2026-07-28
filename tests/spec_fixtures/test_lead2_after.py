#!/usr/bin/env python3
"""Executable CAPA-001 Lead-2 retirement-evidence after-repair oracle.

The oracle is intentionally isolated and Python-stdlib only. Its original
12-case crossing checks remain stable, while its NULL-vacuity checks execute
the current plan/effect/result DDL directly with only unrelated parent
tables reduced to fixtures.

Stable output reports L2-A/B/C/D, the aggregate crossing case count, and a
final empty ``PRAGMA foreign_key_check``.
"""

from __future__ import annotations

import sqlite3
from collections.abc import Callable, Mapping
from pathlib import Path
from typing import Any

from spec_sources import AGENT_FABRIC_HARDENING, read_specs


class OracleFailure(AssertionError):
    """A fixture invariant failed."""


EVIDENCE_FIELDS = (
    "finalized_terminal_evidence_digest",
    "admission_digest",
    "transition_proof_digest",
    "mutation_plan_digest",
    "retirement_evidence_digest",
)

ROOT = Path(__file__).resolve().parents[2]
HARDENING_SPECS = read_specs(AGENT_FABRIC_HARDENING)


def ddl_block(text: str, table: str) -> str:
    start = text.index(f"\n{table}(") + 1
    end = text.index("\n)\n", start) + 2
    return text[start:end]


def create_from_spec(table: str) -> str:
    return f"CREATE TABLE {ddl_block(HARDENING_SPECS, table)};"


SCHEMA = r"""
CREATE TABLE lifecycle_rotation_custody_revisions(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision >= 1),
  disposition_code TEXT NOT NULL,
  terminal_evidence_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  PRIMARY KEY(run_id,agent_id,custody_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
    disposition_code,terminal_evidence_digest,source_ref_digest,journal_digest)
);

CREATE TABLE lifecycle_recovery_retirement_plans(
  retirement_id TEXT PRIMARY KEY,
  revision INTEGER NOT NULL CHECK(revision=1),
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  custody_revision INTEGER NOT NULL,
  custody_source_ref_digest TEXT NOT NULL,
  custody_journal_digest TEXT NOT NULL,
  finalized_disposition TEXT NOT NULL CHECK(finalized_disposition IN
    ('no-effect','superseded','quarantined')),
  finalized_terminal_evidence_digest TEXT NOT NULL,
  admission_digest TEXT NOT NULL,
  transition_proof_json TEXT NOT NULL,
  transition_proof_digest TEXT NOT NULL,
  mutation_plan_json TEXT NOT NULL,
  mutation_plan_digest TEXT NOT NULL,
  retirement_evidence_digest TEXT NOT NULL,
  planned_apply_id TEXT NOT NULL UNIQUE,
  recorded_at TEXT NOT NULL,
  plan_json TEXT NOT NULL,
  retirement_plan_digest TEXT NOT NULL UNIQUE,
  UNIQUE(retirement_id,retirement_plan_digest,planned_apply_id,
    project_session_id,run_id,agent_id,mutation_plan_digest),
  UNIQUE(retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
    custody_id,custody_revision,custody_source_ref_digest,custody_journal_digest,
    finalized_disposition,finalized_terminal_evidence_digest,admission_digest,
    transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
    retirement_plan_digest),
  FOREIGN KEY(project_session_id,run_id,agent_id,custody_id,custody_revision,
      finalized_disposition,finalized_terminal_evidence_digest,
      custody_source_ref_digest,custody_journal_digest)
    REFERENCES lifecycle_rotation_custody_revisions(
      project_session_id,run_id,agent_id,custody_id,revision,disposition_code,
      terminal_evidence_digest,source_ref_digest,journal_digest)
);

CREATE TABLE lifecycle_receipt_batches(
  batch_id TEXT PRIMARY KEY,
  planned_apply_id TEXT NOT NULL UNIQUE,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  transition_kind TEXT NOT NULL
    CHECK(transition_kind='custody-recovery-retirement'),
  mutation_plan_digest TEXT NOT NULL,
  recovery_retirement_id TEXT NOT NULL,
  recovery_retirement_plan_digest TEXT NOT NULL,
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
    transition_kind),
  FOREIGN KEY(recovery_retirement_id,recovery_retirement_plan_digest,
      planned_apply_id,project_session_id,run_id,agent_id,mutation_plan_digest)
    REFERENCES lifecycle_recovery_retirement_plans(
      retirement_id,retirement_plan_digest,planned_apply_id,
      project_session_id,run_id,agent_id,mutation_plan_digest)
);

CREATE TABLE lifecycle_receipt_recovery_retirement_effects(
  batch_id TEXT PRIMARY KEY,
  ordinal INTEGER NOT NULL CHECK(ordinal=1),
  role TEXT NOT NULL CHECK(role='primary'),
  transition_kind TEXT NOT NULL
    CHECK(transition_kind='custody-recovery-retirement'),
  planned_apply_id TEXT NOT NULL,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  retirement_id TEXT NOT NULL UNIQUE,
  retirement_revision INTEGER NOT NULL CHECK(retirement_revision=1),
  retirement_plan_digest TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  custody_revision INTEGER NOT NULL,
  custody_source_ref_digest TEXT NOT NULL,
  custody_journal_digest TEXT NOT NULL,
  finalized_disposition TEXT NOT NULL,
  finalized_terminal_evidence_digest TEXT NOT NULL,
  admission_digest TEXT NOT NULL,
  transition_proof_digest TEXT NOT NULL,
  mutation_plan_digest TEXT NOT NULL,
  retirement_evidence_digest TEXT NOT NULL,
  effect_digest TEXT NOT NULL UNIQUE,
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
    retirement_id,retirement_plan_digest,custody_id,custody_revision,
    custody_source_ref_digest,custody_journal_digest,finalized_disposition,
    finalized_terminal_evidence_digest,admission_digest,
    transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
    effect_digest),
  FOREIGN KEY(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
      transition_kind)
    REFERENCES lifecycle_receipt_batches(
      batch_id,planned_apply_id,project_session_id,run_id,agent_id,
      transition_kind),
  FOREIGN KEY(retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
      custody_id,custody_revision,custody_source_ref_digest,
      custody_journal_digest,finalized_disposition,
      finalized_terminal_evidence_digest,admission_digest,
      transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
      retirement_plan_digest)
    REFERENCES lifecycle_recovery_retirement_plans(
      retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
      custody_id,custody_revision,custody_source_ref_digest,
      custody_journal_digest,finalized_disposition,
      finalized_terminal_evidence_digest,admission_digest,
      transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
      retirement_plan_digest)
);

CREATE TABLE lifecycle_transition_applies(
  apply_id TEXT PRIMARY KEY,
  apply_digest TEXT NOT NULL,
  receipt_batch_id TEXT NOT NULL UNIQUE,
  UNIQUE(apply_id,apply_digest,receipt_batch_id)
);

CREATE TABLE agent_lifecycle_recovery_retirements(
  retirement_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  retirement_plan_digest TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  custody_revision INTEGER NOT NULL,
  custody_source_ref_digest TEXT NOT NULL,
  custody_journal_digest TEXT NOT NULL,
  finalized_disposition TEXT NOT NULL,
  finalized_terminal_evidence_digest TEXT NOT NULL,
  admission_digest TEXT NOT NULL,
  transition_proof_digest TEXT NOT NULL,
  mutation_plan_digest TEXT NOT NULL,
  retirement_evidence_digest TEXT NOT NULL,
  retirement_effect_digest TEXT NOT NULL,
  receipt_batch_id TEXT NOT NULL UNIQUE,
  receipt_apply_id TEXT NOT NULL UNIQUE,
  receipt_apply_digest TEXT NOT NULL,
  retirement_json TEXT NOT NULL,
  retirement_digest TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL,
  FOREIGN KEY(retirement_id,receipt_apply_id,project_session_id,run_id,agent_id,
      custody_id,custody_revision,custody_source_ref_digest,
      custody_journal_digest,finalized_disposition,
      finalized_terminal_evidence_digest,admission_digest,
      transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
      retirement_plan_digest)
    REFERENCES lifecycle_recovery_retirement_plans(
      retirement_id,planned_apply_id,project_session_id,run_id,agent_id,
      custody_id,custody_revision,custody_source_ref_digest,
      custody_journal_digest,finalized_disposition,
      finalized_terminal_evidence_digest,admission_digest,
      transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
      retirement_plan_digest),
  FOREIGN KEY(receipt_batch_id,receipt_apply_id,project_session_id,run_id,agent_id,
      retirement_id,retirement_plan_digest,custody_id,custody_revision,
      custody_source_ref_digest,custody_journal_digest,finalized_disposition,
      finalized_terminal_evidence_digest,admission_digest,
      transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
      retirement_effect_digest)
    REFERENCES lifecycle_receipt_recovery_retirement_effects(
      batch_id,planned_apply_id,project_session_id,run_id,agent_id,retirement_id,
      retirement_plan_digest,custody_id,custody_revision,
      custody_source_ref_digest,custody_journal_digest,finalized_disposition,
      finalized_terminal_evidence_digest,admission_digest,
      transition_proof_digest,mutation_plan_digest,retirement_evidence_digest,
      effect_digest),
  FOREIGN KEY(receipt_apply_id,receipt_apply_digest,receipt_batch_id)
    REFERENCES lifecycle_transition_applies(
      apply_id,apply_digest,receipt_batch_id)
    DEFERRABLE INITIALLY DEFERRED
);
"""


NORMATIVE_SUPPORT_SCHEMA = r"""
CREATE TABLE lifecycle_rotation_custody_revisions(
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  custody_id TEXT NOT NULL,
  revision INTEGER NOT NULL CHECK(revision >= 1),
  disposition_code TEXT NOT NULL,
  terminal_evidence_digest TEXT NOT NULL,
  source_ref_digest TEXT NOT NULL,
  journal_digest TEXT NOT NULL,
  PRIMARY KEY(run_id,agent_id,custody_id,revision),
  UNIQUE(project_session_id,run_id,agent_id,custody_id,revision,
    disposition_code,terminal_evidence_digest,source_ref_digest,journal_digest)
);
"""

NORMATIVE_BATCH_SCHEMA = r"""
CREATE TABLE lifecycle_receipt_batches(
  batch_id TEXT PRIMARY KEY,
  planned_apply_id TEXT NOT NULL UNIQUE,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  transition_kind TEXT NOT NULL
    CHECK(transition_kind='custody-recovery-retirement'),
  mutation_plan_digest TEXT NOT NULL,
  recovery_retirement_id TEXT NOT NULL,
  recovery_retirement_plan_digest TEXT NOT NULL,
  UNIQUE(batch_id,planned_apply_id,project_session_id,run_id,agent_id,
    transition_kind),
  FOREIGN KEY(recovery_retirement_id,recovery_retirement_plan_digest,
      planned_apply_id,project_session_id,run_id,agent_id,mutation_plan_digest)
    REFERENCES lifecycle_recovery_retirement_plans(
      retirement_id,retirement_plan_digest,planned_apply_id,
      project_session_id,run_id,agent_id,mutation_plan_digest)
);
"""

NORMATIVE_APPLY_SCHEMA = r"""
CREATE TABLE lifecycle_transition_applies(
  apply_id TEXT PRIMARY KEY,
  apply_digest TEXT NOT NULL,
  receipt_batch_id TEXT NOT NULL UNIQUE,
  UNIQUE(apply_id,apply_digest,receipt_batch_id)
);
"""

NORMATIVE_SCHEMA = "\n".join(
    (
        NORMATIVE_SUPPORT_SCHEMA,
        create_from_spec("lifecycle_recovery_retirement_plans"),
        NORMATIVE_BATCH_SCHEMA,
        create_from_spec("lifecycle_receipt_recovery_retirement_effects"),
        NORMATIVE_APPLY_SCHEMA,
        create_from_spec("agent_lifecycle_recovery_retirements"),
    )
)


CUSTODY = {
    "project_session_id": "project-session-1",
    "run_id": "run-1",
    "agent_id": "agent-1",
    "custody_id": "custody-1",
    "revision": 7,
    "disposition_code": "no-effect",
    "terminal_evidence_digest": "terminal-evidence-1",
    "source_ref_digest": "custody-source-ref-1",
    "journal_digest": "custody-journal-1",
}

PLAN = {
    "retirement_id": "retirement-1",
    "revision": 1,
    "project_session_id": "project-session-1",
    "run_id": "run-1",
    "agent_id": "agent-1",
    "custody_id": "custody-1",
    "custody_revision": 7,
    "custody_source_ref_digest": "custody-source-ref-1",
    "custody_journal_digest": "custody-journal-1",
    "finalized_disposition": "no-effect",
    "finalized_terminal_evidence_digest": "terminal-evidence-1",
    "admission_digest": "admission-1",
    "transition_proof_json": "{}",
    "transition_proof_digest": "transition-proof-1",
    "mutation_plan_json": "{}",
    "mutation_plan_digest": "mutation-plan-1",
    "retirement_evidence_digest": "retirement-evidence-1",
    "planned_apply_id": "apply-1",
    "recorded_at": "2026-07-14T00:00:00Z",
    "plan_json": "{}",
    "retirement_plan_digest": "retirement-plan-1",
}

BATCH = {
    "batch_id": "batch-1",
    "planned_apply_id": "apply-1",
    "project_session_id": "project-session-1",
    "run_id": "run-1",
    "agent_id": "agent-1",
    "transition_kind": "custody-recovery-retirement",
    "mutation_plan_digest": "mutation-plan-1",
    "recovery_retirement_id": "retirement-1",
    "recovery_retirement_plan_digest": "retirement-plan-1",
}

EFFECT = {
    "batch_id": "batch-1",
    "ordinal": 1,
    "role": "primary",
    "transition_kind": "custody-recovery-retirement",
    "planned_apply_id": "apply-1",
    "project_session_id": "project-session-1",
    "run_id": "run-1",
    "agent_id": "agent-1",
    "retirement_id": "retirement-1",
    "retirement_revision": 1,
    "retirement_plan_digest": "retirement-plan-1",
    "custody_id": "custody-1",
    "custody_revision": 7,
    "custody_source_ref_digest": "custody-source-ref-1",
    "custody_journal_digest": "custody-journal-1",
    "finalized_disposition": "no-effect",
    "finalized_terminal_evidence_digest": "terminal-evidence-1",
    "admission_digest": "admission-1",
    "transition_proof_digest": "transition-proof-1",
    "mutation_plan_digest": "mutation-plan-1",
    "retirement_evidence_digest": "retirement-evidence-1",
    "effect_digest": "retirement-effect-1",
}

RESULT = {
    "retirement_id": "retirement-1",
    "project_session_id": "project-session-1",
    "run_id": "run-1",
    "agent_id": "agent-1",
    "retirement_plan_digest": "retirement-plan-1",
    "custody_id": "custody-1",
    "custody_revision": 7,
    "custody_source_ref_digest": "custody-source-ref-1",
    "custody_journal_digest": "custody-journal-1",
    "finalized_disposition": "no-effect",
    "finalized_terminal_evidence_digest": "terminal-evidence-1",
    "admission_digest": "admission-1",
    "transition_proof_digest": "transition-proof-1",
    "mutation_plan_digest": "mutation-plan-1",
    "retirement_evidence_digest": "retirement-evidence-1",
    "retirement_effect_digest": "retirement-effect-1",
    "receipt_batch_id": "batch-1",
    "receipt_apply_id": "apply-1",
    "receipt_apply_digest": "apply-digest-1",
    "retirement_json": "{}",
    "retirement_digest": "retirement-result-1",
    "created_at": "2026-07-14T00:00:01Z",
}

APPLY = {
    "apply_id": "apply-1",
    "apply_digest": "apply-digest-1",
    "receipt_batch_id": "batch-1",
}

# Every displayed member of these three closed rows is required. Exercising the
# dictionaries directly keeps the extracted-DDL NULL audit aligned with the
# executable exact-row fixtures as their codecs evolve.
PLAN_CHAIN_FIELDS = tuple(PLAN)
EFFECT_CHAIN_FIELDS = tuple(EFFECT)
RESULT_CHAIN_FIELDS = tuple(RESULT)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise OracleFailure(message)


def insert_row(
    connection: sqlite3.Connection,
    table: str,
    row: Mapping[str, Any],
) -> None:
    columns = ",".join(row)
    placeholders = ",".join("?" for _ in row)
    connection.execute(
        f"INSERT INTO {table} ({columns}) VALUES ({placeholders})",
        tuple(row.values()),
    )


def assert_foreign_keys_clean(connection: sqlite3.Connection) -> None:
    violations = list(connection.execute("PRAGMA foreign_key_check"))
    require(violations == [], f"foreign_key_check violations: {violations}")


def expect_foreign_key_rejection(
    connection: sqlite3.Connection,
    operation: Callable[[], None],
) -> None:
    try:
        operation()
    except sqlite3.IntegrityError as error:
        require(
            str(error) == "FOREIGN KEY constraint failed",
            f"wrong SQLite rejection: {error}",
        )
        connection.rollback()
        return
    raise OracleFailure("crossed tuple was accepted")


def expect_not_null_rejection(
    connection: sqlite3.Connection,
    operation: Callable[[], None],
) -> None:
    try:
        operation()
    except sqlite3.IntegrityError as error:
        error_name = getattr(error, "sqlite_errorname", "")
        require(
            error_name == "SQLITE_CONSTRAINT_NOTNULL",
            f"wrong SQLite rejection: {error_name}: {error}",
        )
        connection.rollback()
        return
    raise OracleFailure("NULL composite-key component was accepted")


def baseline(*, with_effect: bool) -> sqlite3.Connection:
    connection = sqlite3.connect(":memory:")
    connection.execute("PRAGMA foreign_keys=ON")
    require(
        connection.execute("PRAGMA foreign_keys").fetchone() == (1,),
        "foreign keys were not enabled",
    )
    connection.executescript(SCHEMA)
    insert_row(connection, "lifecycle_rotation_custody_revisions", CUSTODY)
    insert_row(connection, "lifecycle_recovery_retirement_plans", PLAN)
    insert_row(connection, "lifecycle_receipt_batches", BATCH)
    if with_effect:
        insert_row(
            connection,
            "lifecycle_receipt_recovery_retirement_effects",
            EFFECT,
        )
    connection.commit()
    assert_foreign_keys_clean(connection)
    return connection


def insert_exact_result_and_apply(connection: sqlite3.Connection) -> None:
    connection.execute("BEGIN")
    insert_row(connection, "agent_lifecycle_recovery_retirements", RESULT)
    # The apply marker is deliberately inserted last, as specified.
    insert_row(connection, "lifecycle_transition_applies", APPLY)
    connection.commit()


def normative_baseline(*, with_effect: bool) -> sqlite3.Connection:
    connection = sqlite3.connect(":memory:")
    connection.execute("PRAGMA foreign_keys=ON")
    connection.executescript(NORMATIVE_SCHEMA)
    insert_row(connection, "lifecycle_rotation_custody_revisions", CUSTODY)
    insert_row(connection, "lifecycle_recovery_retirement_plans", PLAN)
    insert_row(connection, "lifecycle_receipt_batches", BATCH)
    if with_effect:
        insert_row(
            connection,
            "lifecycle_receipt_recovery_retirement_effects",
            EFFECT,
        )
    connection.commit()
    assert_foreign_keys_clean(connection)
    return connection


def l2_a_exact_tuple_is_accepted() -> None:
    connection = baseline(with_effect=True)
    try:
        insert_exact_result_and_apply(connection)
        require(
            connection.execute(
                "SELECT COUNT(*) FROM agent_lifecycle_recovery_retirements"
            ).fetchone()
            == (1,),
            "exact retirement result was not persisted",
        )
        assert_foreign_keys_clean(connection)
    finally:
        connection.close()


def l2_b_effect_mutations_are_rejected() -> None:
    for field in EVIDENCE_FIELDS:
        connection = baseline(with_effect=False)
        try:
            mutant = dict(EFFECT)
            mutant[field] = f"crossed-{field}"
            expect_foreign_key_rejection(
                connection,
                lambda mutant=mutant: insert_row(
                    connection,
                    "lifecycle_receipt_recovery_retirement_effects",
                    mutant,
                ),
            )
            require(
                connection.execute(
                    "SELECT COUNT(*) "
                    "FROM lifecycle_receipt_recovery_retirement_effects"
                ).fetchone()
                == (0,),
                f"mutated effect survived for {field}",
            )
            assert_foreign_keys_clean(connection)
        finally:
            connection.close()


def l2_c_result_mutations_are_rejected() -> None:
    mutations = tuple(EVIDENCE_FIELDS) + ("retirement_effect_digest",)
    for field in mutations:
        connection = baseline(with_effect=True)
        try:
            mutant = dict(RESULT)
            mutant[field] = f"crossed-{field}"
            expect_foreign_key_rejection(
                connection,
                lambda mutant=mutant: insert_row(
                    connection,
                    "agent_lifecycle_recovery_retirements",
                    mutant,
                ),
            )
            require(
                connection.execute(
                    "SELECT COUNT(*) FROM agent_lifecycle_recovery_retirements"
                ).fetchone()
                == (0,),
                f"mutated result survived for {field}",
            )
            assert_foreign_keys_clean(connection)
        finally:
            connection.close()


def main() -> None:
    l2_a_exact_tuple_is_accepted()
    print("L2-A PASS exact plan/effect/result tuple accepted")

    l2_b_effect_mutations_are_rejected()
    print("L2-B PASS effect tuple mutations rejected 5/5")

    l2_c_result_mutations_are_rejected()
    print("L2-C PASS result/effect crossings rejected 6/6")
    print("LEAD2-AFTER PASS cases=12")

    final_connection = baseline(with_effect=True)
    try:
        insert_exact_result_and_apply(final_connection)
        assert_foreign_keys_clean(final_connection)
    finally:
        final_connection.close()
    print("PRAGMA foreign_key_check PASS rows=0")


def test_lead2_after_oracle(capsys) -> None:
    main()
    output = capsys.readouterr().out
    assert "LEAD2-AFTER PASS cases=12" in output
    assert "PRAGMA foreign_key_check PASS rows=0" in output


if __name__ == "__main__":
    main()
