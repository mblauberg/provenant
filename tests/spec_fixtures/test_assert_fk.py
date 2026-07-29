#!/usr/bin/env python3
"""Focused tests for the structural foreign-key assertion oracle."""

from __future__ import annotations

import sqlite3
import unittest

from assert_fk import ForeignKeySpec, assert_fk_rejected


CHILD_TO_PARENT_A = ForeignKeySpec(
    child_table="child",
    child_columns=("parent_a_id",),
    parent_table="parent_a",
    parent_columns=("id",),
)


def database() -> sqlite3.Connection:
    db = sqlite3.connect(":memory:", isolation_level=None)
    db.execute("PRAGMA foreign_keys=ON")
    db.executescript(
        """
        CREATE TABLE parent_a(id INTEGER PRIMARY KEY);
        CREATE TABLE parent_b(id INTEGER PRIMARY KEY);
        CREATE TABLE child(
          id INTEGER PRIMARY KEY,
          parent_a_id INTEGER NOT NULL,
          parent_b_id INTEGER NOT NULL,
          FOREIGN KEY(parent_a_id) REFERENCES parent_a(id),
          FOREIGN KEY(parent_b_id) REFERENCES parent_b(id)
        );
        INSERT INTO parent_a VALUES(1);
        INSERT INTO parent_b VALUES(1);
        """
    )
    return db


class StructuralForeignKeyAssertionTests(unittest.TestCase):
    def test_accepts_only_the_expected_structural_foreign_key(self) -> None:
        db = database()
        try:
            assert_fk_rejected(
                db,
                invalid_operation=lambda connection: connection.execute(
                    "INSERT INTO child VALUES(1, 999, 1)"
                ),
                positive_control=lambda connection: connection.execute(
                    "INSERT INTO child VALUES(1, 1, 1)"
                ),
                expected=frozenset({CHILD_TO_PARENT_A}),
            )
            self.assertEqual([(1, 1, 1)], db.execute("SELECT * FROM child").fetchall())
        finally:
            db.close()

    def test_rejects_a_different_foreign_key_in_the_same_row(self) -> None:
        db = database()
        try:
            with self.assertRaisesRegex(
                AssertionError,
                "observed foreign keys did not equal expected",
            ):
                assert_fk_rejected(
                    db,
                    invalid_operation=lambda connection: connection.execute(
                        "INSERT INTO child VALUES(1, 1, 999)"
                    ),
                    positive_control=lambda connection: connection.execute(
                        "INSERT INTO child VALUES(1, 1, 1)"
                    ),
                    expected=frozenset({CHILD_TO_PARENT_A}),
                )
            self.assertFalse(db.in_transaction)
            self.assertEqual([], db.execute("SELECT * FROM child").fetchall())
        finally:
            db.close()


if __name__ == "__main__":
    unittest.main()
