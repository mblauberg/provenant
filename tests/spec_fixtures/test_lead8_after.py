#!/usr/bin/env python3
"""Executable Lead 8 after-repair SQLite race oracle.

The source-head table and every Lead 8 trigger are extracted from the recovery contract.
Only unrelated issue, handoff and revocation columns are reduced to stubs.  The
oracle intentionally uses a file-backed database and separate connections for
writer races.  Every writer starts with ``BEGIN IMMEDIATE``; the second writer
must observe the first committed winner before its trigger/FK checks.

Run:
    python3 tests/spec_fixtures/test_lead8_after.py
"""

from __future__ import annotations

import sqlite3
import tempfile
import threading
import unittest
from collections.abc import Callable
from pathlib import Path

from assert_fk import ForeignKeySpec, assert_fk_rejected
from spec_sources import AGENT_FABRIC_HARDENING, read_specs


SOURCE_BUSY = "LIFECYCLE_RECOVERY_SOURCE_BUSY"
ISSUE_REVOKED = "LIFECYCLE_RECOVERY_ISSUE_REVOKED"
ISSUE_EXPIRED = "LIFECYCLE_RECOVERY_ISSUE_EXPIRED"
ISSUE_COMMIT_PENDING = "LIFECYCLE_RECOVERY_ISSUE_COMMIT_PENDING"
HEAD_DELETE_DENIED = "LIFECYCLE_RECOVERY_SOURCE_HEAD_DELETE_DENIED"
HEAD_REINSERT_DENIED = "LIFECYCLE_RECOVERY_SOURCE_HEAD_REINSERT_DENIED"
ISSUE_IMMUTABLE = "LIFECYCLE_RECOVERY_ISSUE_IMMUTABLE"
HANDOFF_IMMUTABLE = "LIFECYCLE_RECOVERY_HANDOFF_IMMUTABLE"
HANDOFF_REINSERT_DENIED = "LIFECYCLE_RECOVERY_HANDOFF_REINSERT_DENIED"
REVOCATION_IMMUTABLE = "LIFECYCLE_RECOVERY_REVOCATION_IMMUTABLE"
REVOCATION_REINSERT_DENIED = "LIFECYCLE_RECOVERY_REVOCATION_REINSERT_DENIED"

PS = "project-session-1"
RUN = "run-1"
AGENT = "agent-1"
SOURCE_KIND = "custody"
SOURCE_REF = "sha256:source-1"

T0 = "2026-07-14T00:00:00.000Z"
T5 = "2026-07-14T00:00:05.000Z"
T6 = "2026-07-14T00:00:06.000Z"
T10 = "2026-07-14T00:00:10.000Z"
T11 = "2026-07-14T00:00:11.000Z"
T20 = "2026-07-14T00:00:20.000Z"
T30 = "2026-07-14T00:00:30.000Z"


ROOT = Path(__file__).resolve().parents[2]
HARDENING_SPECS = read_specs(AGENT_FABRIC_HARDENING)


def ddl_block(text: str, table: str) -> str:
    start = text.index(f"\n{table}(") + 1
    end = text.index("\n)\n", start) + 2
    return text[start:end]


def trigger_sql(text: str, name: str) -> str:
    start = text.index(f"CREATE TRIGGER {name}\n")
    end = text.index("\nEND;", start) + len("\nEND;")
    return text[start:end]


LEAD8_TRIGGERS = (
    "lifecycle_recovery_issue_claim_source",
    "lifecycle_recovery_source_head_reinsert_denied",
    "lifecycle_recovery_source_head_update_guard",
    "lifecycle_recovery_source_head_delete_guard",
    "lifecycle_recovery_handoff_guard",
    "lifecycle_recovery_handoff_reinsert_denied",
    "lifecycle_recovery_revocation_guard",
    "lifecycle_recovery_revocation_reinsert_denied",
    "lifecycle_recovery_issue_update_denied",
    "lifecycle_recovery_issue_delete_denied",
    "lifecycle_recovery_handoff_update_denied",
    "lifecycle_recovery_handoff_delete_denied",
    "lifecycle_recovery_revocation_update_denied",
    "lifecycle_recovery_revocation_delete_denied",
)


FIXTURE_SCHEMA_SQL = """
PRAGMA foreign_keys = ON;

CREATE TABLE agent_lifecycle_recovery_capability_issues(
  issue_id TEXT PRIMARY KEY,
  project_session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  recovery_source_kind TEXT NOT NULL CHECK(
    recovery_source_kind IN ('custody','generation-loss')),
  recovery_source_ref_digest TEXT NOT NULL,
  source_journal_digest TEXT NOT NULL,
  issued_at TEXT NOT NULL CHECK(
    length(issued_at)=24 AND substr(issued_at,24,1)='Z'),
  expires_at TEXT NOT NULL CHECK(
    length(expires_at)=24 AND substr(expires_at,24,1)='Z'),
  UNIQUE(issue_id,project_session_id,run_id,agent_id,recovery_source_kind,
    recovery_source_ref_digest,source_journal_digest),
  CHECK(issued_at < expires_at)
) STRICT;

CREATE TABLE lifecycle_fresh_recovery_handoffs(
  handoff_id TEXT PRIMARY KEY,
  issue_id TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL CHECK(
    length(created_at)=24 AND substr(created_at,24,1)='Z'),
  FOREIGN KEY(issue_id)
    REFERENCES agent_lifecycle_recovery_source_heads(issue_id)
) STRICT;

CREATE TABLE agent_lifecycle_recovery_issue_revocations(
  issue_id TEXT PRIMARY KEY,
  revocation_kind TEXT NOT NULL CHECK(
    revocation_kind IN ('operator-revoked','source-stale')),
  evidence_digest TEXT NOT NULL,
  revoked_at TEXT NOT NULL CHECK(
    length(revoked_at)=24 AND substr(revoked_at,24,1)='Z'),
  FOREIGN KEY(issue_id)
    REFERENCES agent_lifecycle_recovery_capability_issues(issue_id)
) STRICT;
"""


SCHEMA_SQL = "\n\n".join(
    (
        FIXTURE_SCHEMA_SQL.split(
            "CREATE TABLE lifecycle_fresh_recovery_handoffs", maxsplit=1
        )[0],
        f"CREATE TABLE {ddl_block(HARDENING_SPECS, 'agent_lifecycle_recovery_source_heads')};",
        "CREATE TABLE lifecycle_fresh_recovery_handoffs"
        + FIXTURE_SCHEMA_SQL.split(
            "CREATE TABLE lifecycle_fresh_recovery_handoffs", maxsplit=1
        )[1],
        *(trigger_sql(HARDENING_SPECS, name) for name in LEAD8_TRIGGERS),
    )
)


def insert_issue(
    connection: sqlite3.Connection,
    issue_id: str,
    issued_at: str,
    expires_at: str,
    journal_digest: str,
) -> None:
    connection.execute(
        """
        INSERT INTO agent_lifecycle_recovery_capability_issues(
          issue_id,project_session_id,run_id,agent_id,recovery_source_kind,
          recovery_source_ref_digest,source_journal_digest,issued_at,expires_at)
        VALUES(?,?,?,?,?,?,?,?,?)
        """,
        (
            issue_id,
            PS,
            RUN,
            AGENT,
            SOURCE_KIND,
            SOURCE_REF,
            journal_digest,
            issued_at,
            expires_at,
        ),
    )


def insert_handoff(
    connection: sqlite3.Connection,
    handoff_id: str,
    issue_id: str,
    created_at: str,
) -> None:
    connection.execute(
        """
        INSERT INTO lifecycle_fresh_recovery_handoffs(
          handoff_id,issue_id,created_at)
        VALUES(?,?,?)
        """,
        (handoff_id, issue_id, created_at),
    )


def insert_revocation(
    connection: sqlite3.Connection,
    issue_id: str,
    revoked_at: str,
) -> None:
    connection.execute(
        """
        INSERT INTO agent_lifecycle_recovery_issue_revocations(
          issue_id,revocation_kind,evidence_digest,revoked_at)
        VALUES(?,'operator-revoked',?,?)
        """,
        (issue_id, f"sha256:revoke-{issue_id}", revoked_at),
    )


Writer = Callable[[sqlite3.Connection], None]


class Lead8AfterRepairTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temporary_directory = tempfile.TemporaryDirectory(
            prefix="capa-001-lead8-"
        )
        self.database_path = (
            Path(self._temporary_directory.name) / "lead8.sqlite3"
        )
        connection = self.connect()
        try:
            connection.execute("PRAGMA recursive_triggers=OFF")
            self.assertEqual(
                connection.execute("PRAGMA recursive_triggers").fetchone(), (0,)
            )
            connection.executescript(SCHEMA_SQL)
        finally:
            connection.close()

    def tearDown(self) -> None:
        self._temporary_directory.cleanup()

    def connect(self) -> sqlite3.Connection:
        connection = sqlite3.connect(
            self.database_path,
            timeout=5.0,
            isolation_level=None,
        )
        connection.execute("PRAGMA foreign_keys=ON")
        connection.execute("PRAGMA busy_timeout=5000")
        return connection

    def write(self, writer: Writer) -> None:
        connection = self.connect()
        try:
            connection.execute("BEGIN IMMEDIATE")
            writer(connection)
            connection.commit()
        except BaseException:
            if connection.in_transaction:
                connection.rollback()
            raise
        finally:
            connection.close()

    def seed_issue(
        self,
        issue_id: str = "issue-1",
        issued_at: str = T0,
        expires_at: str = T20,
        journal_digest: str = "sha256:journal-1",
    ) -> None:
        self.write(
            lambda connection: insert_issue(
                connection,
                issue_id,
                issued_at,
                expires_at,
                journal_digest,
            )
        )

    def race_after_first_commit(
        self,
        first_writer: Writer,
        second_writer: Writer,
    ) -> BaseException | None:
        """Hold writer one's RESERVED lock until writer two is waiting."""

        first = self.connect()
        first.execute("BEGIN IMMEDIATE")
        first_writer(first)

        attempting = threading.Event()
        acquired = threading.Event()
        finished = threading.Event()
        outcome: dict[str, BaseException | None] = {}

        def run_second_writer() -> None:
            second = self.connect()
            try:
                attempting.set()
                second.execute("BEGIN IMMEDIATE")
                acquired.set()
                second_writer(second)
                second.commit()
                outcome["error"] = None
            except BaseException as error:  # captured for main-thread assertion
                if second.in_transaction:
                    second.rollback()
                outcome["error"] = error
            finally:
                second.close()
                finished.set()

        thread = threading.Thread(target=run_second_writer, daemon=True)
        thread.start()
        self.assertTrue(attempting.wait(1.0), "second writer never attempted")
        self.assertFalse(
            acquired.wait(0.10),
            "BEGIN IMMEDIATE did not serialize the second writer",
        )
        first.commit()
        first.close()
        self.assertTrue(finished.wait(3.0), "second writer did not finish")
        thread.join(timeout=0.1)
        return outcome["error"]

    def assert_integrity_error(
        self,
        error: BaseException | None,
        exact_message: str,
    ) -> None:
        self.assertIsInstance(error, sqlite3.IntegrityError)
        self.assertEqual(str(error), exact_message)

    def current_head(self) -> tuple[str, str, int]:
        connection = self.connect()
        try:
            row = connection.execute(
                """
                SELECT issue_id,source_journal_digest,head_revision
                FROM agent_lifecycle_recovery_source_heads
                WHERE project_session_id=? AND run_id=? AND agent_id=?
                  AND recovery_source_kind=?
                  AND recovery_source_ref_digest=?
                """,
                (PS, RUN, AGENT, SOURCE_KIND, SOURCE_REF),
            ).fetchone()
            self.assertIsNotNone(row)
            return row  # type: ignore[return-value]
        finally:
            connection.close()

    def assert_foreign_keys_clean(self) -> None:
        connection = self.connect()
        try:
            self.assertEqual(connection.execute("PRAGMA foreign_key_check").fetchall(), [])
        finally:
            connection.close()

    def test_handoff_before_reissue_blocks_reissue_after_serialization(self) -> None:
        self.seed_issue(expires_at=T10)

        error = self.race_after_first_commit(
            lambda connection: insert_handoff(
                connection, "handoff-1", "issue-1", T5
            ),
            lambda connection: insert_issue(
                connection, "issue-2", T11, T20, "sha256:journal-2"
            ),
        )

        self.assert_integrity_error(error, SOURCE_BUSY)
        self.assertEqual(self.current_head(), ("issue-1", "sha256:journal-1", 1))
        connection = self.connect()
        try:
            self.assertEqual(
                connection.execute(
                    "SELECT issue_id FROM agent_lifecycle_recovery_capability_issues"
                    " ORDER BY issue_id"
                ).fetchall(),
                [("issue-1",)],
            )
        finally:
            connection.close()
        self.assert_foreign_keys_clean()

    def test_reissue_before_old_handoff_blocks_stale_handoff(self) -> None:
        self.seed_issue(expires_at=T10)

        first = self.connect()
        first.execute("BEGIN IMMEDIATE")
        insert_issue(first, "issue-2", T10, T20, "sha256:journal-2")

        attempting = threading.Event()
        finished = threading.Event()
        outcome: dict[str, BaseException | None] = {}

        def run_structural_oracle() -> None:
            second = self.connect()
            try:
                attempting.set()
                assert_fk_rejected(
                    second,
                    # Deliberately forged/backdated: even if the expiry
                    # predicate is bypassed, the current-head FK identifies
                    # the stale issue structurally after writer one commits.
                    invalid_operation=lambda connection: insert_handoff(
                        connection,
                        "handoff-old",
                        "issue-1",
                        T5,
                    ),
                    positive_control=lambda connection: insert_handoff(
                        connection,
                        "handoff-current",
                        "issue-2",
                        T10,
                    ),
                    expected=frozenset(
                        {
                            ForeignKeySpec(
                                "lifecycle_fresh_recovery_handoffs",
                                ("issue_id",),
                                "agent_lifecycle_recovery_source_heads",
                                ("issue_id",),
                            )
                        }
                    ),
                )
                outcome["error"] = None
            except BaseException as error:
                outcome["error"] = error
            finally:
                second.close()
                finished.set()

        thread = threading.Thread(target=run_structural_oracle, daemon=True)
        thread.start()
        self.assertTrue(attempting.wait(1.0), "second writer never attempted")
        self.assertFalse(
            finished.wait(0.10),
            "stale handoff write did not serialize behind the reissue",
        )
        first.commit()
        first.close()
        self.assertTrue(finished.wait(3.0), "structural oracle did not finish")
        thread.join(timeout=0.1)
        if outcome["error"] is not None:
            raise outcome["error"]

        self.assertEqual(self.current_head(), ("issue-2", "sha256:journal-2", 2))
        connection = self.connect()
        try:
            self.assertEqual(
                connection.execute(
                    "SELECT handoff_id,issue_id "
                    "FROM lifecycle_fresh_recovery_handoffs"
                ).fetchone(),
                ("handoff-current", "issue-2"),
            )
        finally:
            connection.close()
        self.assert_foreign_keys_clean()

    def test_revocation_then_handoff_serializes_to_revoked_error(self) -> None:
        self.seed_issue()

        error = self.race_after_first_commit(
            lambda connection: insert_revocation(connection, "issue-1", T5),
            lambda connection: insert_handoff(
                connection, "handoff-1", "issue-1", T6
            ),
        )

        self.assert_integrity_error(error, ISSUE_REVOKED)
        for statement in (
            "UPDATE agent_lifecycle_recovery_issue_revocations "
            "SET evidence_digest='sha256:forged' WHERE issue_id='issue-1'",
            "DELETE FROM agent_lifecycle_recovery_issue_revocations "
            "WHERE issue_id='issue-1'",
        ):
            with self.assertRaisesRegex(
                sqlite3.IntegrityError, f"^{REVOCATION_IMMUTABLE}$"
            ):
                self.write(lambda connection, sql=statement: connection.execute(sql))
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, f"^{REVOCATION_REINSERT_DENIED}$"
        ):
            self.write(
                lambda connection: connection.execute(
                    """
                    INSERT OR REPLACE INTO agent_lifecycle_recovery_issue_revocations(
                      issue_id,revocation_kind,evidence_digest,revoked_at)
                    VALUES('issue-1','operator-revoked','sha256:replaced',?)
                    """,
                    (T6,),
                )
            )
        self.assert_foreign_keys_clean()

    def test_handoff_then_revocation_serializes_to_commit_pending_error(self) -> None:
        self.seed_issue()

        error = self.race_after_first_commit(
            lambda connection: insert_handoff(
                connection, "handoff-1", "issue-1", T5
            ),
            lambda connection: insert_revocation(connection, "issue-1", T6),
        )

        self.assert_integrity_error(error, ISSUE_COMMIT_PENDING)
        for statement in (
            "UPDATE lifecycle_fresh_recovery_handoffs "
            "SET created_at='2026-07-14T00:00:07.000Z' "
            "WHERE issue_id='issue-1'",
            "DELETE FROM lifecycle_fresh_recovery_handoffs "
            "WHERE issue_id='issue-1'",
        ):
            with self.assertRaisesRegex(
                sqlite3.IntegrityError, f"^{HANDOFF_IMMUTABLE}$"
            ):
                self.write(lambda connection, sql=statement: connection.execute(sql))
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, f"^{HANDOFF_REINSERT_DENIED}$"
        ):
            self.write(
                lambda connection: connection.execute(
                    """
                    INSERT OR REPLACE INTO lifecycle_fresh_recovery_handoffs(
                      handoff_id,issue_id,created_at)
                    VALUES('handoff-replaced','issue-1',?)
                    """,
                    (T6,),
                )
            )
        self.assert_foreign_keys_clean()

    def test_active_source_reissue_has_stable_busy_error(self) -> None:
        self.seed_issue()

        with self.assertRaisesRegex(sqlite3.IntegrityError, f"^{SOURCE_BUSY}$"):
            self.write(
                lambda connection: insert_issue(
                    connection, "issue-2", T5, T30, "sha256:journal-2"
                )
            )

        self.assertEqual(self.current_head(), ("issue-1", "sha256:journal-1", 1))
        for statement in (
            "UPDATE agent_lifecycle_recovery_capability_issues "
            "SET expires_at='2026-07-14T00:00:30.000Z' "
            "WHERE issue_id='issue-1'",
            "DELETE FROM agent_lifecycle_recovery_capability_issues "
            "WHERE issue_id='issue-1'",
        ):
            with self.assertRaisesRegex(
                sqlite3.IntegrityError, f"^{ISSUE_IMMUTABLE}$"
            ):
                self.write(lambda connection, sql=statement: connection.execute(sql))

    def test_handoff_at_expiry_boundary_has_stable_expired_error(self) -> None:
        self.seed_issue(expires_at=T10)

        with self.assertRaisesRegex(sqlite3.IntegrityError, f"^{ISSUE_EXPIRED}$"):
            self.write(
                lambda connection: insert_handoff(
                    connection, "handoff-1", "issue-1", T10
                )
            )

    def test_revoked_pre_handoff_issue_can_be_reissued(self) -> None:
        self.seed_issue()
        self.write(lambda connection: insert_revocation(connection, "issue-1", T5))
        self.write(
            lambda connection: insert_issue(
                connection, "issue-2", T6, T30, "sha256:journal-2"
            )
        )

        self.assertEqual(self.current_head(), ("issue-2", "sha256:journal-2", 2))
        self.assert_foreign_keys_clean()

    def test_expired_pre_handoff_issue_can_be_reissued_at_boundary(self) -> None:
        self.seed_issue(expires_at=T10)
        self.write(
            lambda connection: insert_issue(
                connection, "issue-2", T10, T20, "sha256:journal-2"
            )
        )

        self.assertEqual(self.current_head(), ("issue-2", "sha256:journal-2", 2))
        self.assert_foreign_keys_clean()

    def test_source_head_has_no_copied_clocks_and_delete_is_denied(self) -> None:
        self.seed_issue()
        connection = self.connect()
        try:
            columns = {
                row[1]
                for row in connection.execute(
                    "PRAGMA table_info(agent_lifecycle_recovery_source_heads)"
                )
            }
            self.assertNotIn("issued_at", columns)
            self.assertNotIn("expires_at", columns)
            self.assertEqual(
                columns,
                {
                    "project_session_id",
                    "run_id",
                    "agent_id",
                    "recovery_source_kind",
                    "recovery_source_ref_digest",
                    "issue_id",
                    "source_journal_digest",
                    "head_revision",
                },
            )
        finally:
            connection.close()

        with self.assertRaisesRegex(
            sqlite3.IntegrityError, f"^{HEAD_DELETE_DENIED}$"
        ):
            self.write(
                lambda connection: connection.execute(
                    "DELETE FROM agent_lifecycle_recovery_source_heads"
                )
            )
        with self.assertRaisesRegex(
            sqlite3.IntegrityError, f"^{HEAD_REINSERT_DENIED}$"
        ):
            self.write(
                lambda connection: connection.execute(
                    """
                    INSERT OR REPLACE INTO agent_lifecycle_recovery_source_heads(
                      project_session_id,run_id,agent_id,recovery_source_kind,
                      recovery_source_ref_digest,issue_id,source_journal_digest,
                      head_revision)
                    VALUES(?,?,?,?,?,?,?,99)
                    """,
                    (
                        PS,
                        RUN,
                        AGENT,
                        SOURCE_KIND,
                        SOURCE_REF,
                        "issue-1",
                        "sha256:journal-1",
                    ),
                )
            )

    def test_direct_sql_cannot_rewind_head_to_older_revoked_issue(self) -> None:
        self.seed_issue()
        self.write(lambda connection: insert_revocation(connection, "issue-1", T5))
        self.write(
            lambda connection: insert_issue(
                connection, "issue-2", T6, T30, "sha256:journal-2"
            )
        )
        self.write(lambda connection: insert_revocation(connection, "issue-2", T10))

        with self.assertRaisesRegex(sqlite3.IntegrityError, f"^{SOURCE_BUSY}$"):
            self.write(
                lambda connection: connection.execute(
                    """
                    UPDATE agent_lifecycle_recovery_source_heads
                    SET issue_id='issue-1',
                        source_journal_digest='sha256:journal-1',
                        head_revision=head_revision+1
                    WHERE project_session_id=? AND run_id=? AND agent_id=?
                      AND recovery_source_kind=?
                      AND recovery_source_ref_digest=?
                    """,
                    (PS, RUN, AGENT, SOURCE_KIND, SOURCE_REF),
                )
            )

        self.assertEqual(self.current_head(), ("issue-2", "sha256:journal-2", 2))
        self.assert_foreign_keys_clean()


if __name__ == "__main__":
    unittest.main(verbosity=2)
