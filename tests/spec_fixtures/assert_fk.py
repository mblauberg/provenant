"""Structural assertions for SQLite foreign-key rejection tests."""

from __future__ import annotations

import sqlite3
from collections.abc import Callable
from dataclasses import dataclass


@dataclass(frozen=True)
class ForeignKeySpec:
    child_table: str
    child_columns: tuple[str, ...]
    parent_table: str
    parent_columns: tuple[str, ...]


def _quote_identifier(identifier: str) -> str:
    return '"' + identifier.replace('"', '""') + '"'


def _foreign_key_spec(
    db: sqlite3.Connection,
    *,
    child_table: str,
    parent_table: str,
    foreign_key_id: int,
) -> ForeignKeySpec:
    rows = [
        row
        for row in db.execute(
            f"PRAGMA foreign_key_list({_quote_identifier(child_table)})"
        )
        if row[0] == foreign_key_id
    ]
    if not rows:
        raise AssertionError(
            "foreign_key_check returned an unknown foreign-key id: "
            f"table={child_table!r}, parent={parent_table!r}, id={foreign_key_id}"
        )
    rows.sort(key=lambda row: row[1])
    listed_parent_tables = {row[2] for row in rows}
    if listed_parent_tables != {parent_table}:
        raise AssertionError(
            "foreign_key_check parent did not match foreign_key_list: "
            f"table={child_table!r}, check_parent={parent_table!r}, "
            f"listed_parents={listed_parent_tables!r}"
        )
    return ForeignKeySpec(
        child_table=child_table,
        child_columns=tuple(row[3] for row in rows),
        parent_table=parent_table,
        parent_columns=tuple(row[4] for row in rows),
    )


def _observed_foreign_keys(
    db: sqlite3.Connection,
) -> frozenset[ForeignKeySpec]:
    return frozenset(
        _foreign_key_spec(
            db,
            child_table=child_table,
            parent_table=parent_table,
            foreign_key_id=foreign_key_id,
        )
        for child_table, _rowid, parent_table, foreign_key_id
        in db.execute("PRAGMA foreign_key_check")
    )


def assert_fk_rejected(
    db: sqlite3.Connection,
    *,
    invalid_operation: Callable[[sqlite3.Connection], None],
    positive_control: Callable[[sqlite3.Connection], None],
    expected: frozenset[ForeignKeySpec],
) -> None:
    """Prove that an operation is rejected by exactly the expected SQLite FKs."""

    if db.execute("PRAGMA foreign_keys").fetchone() != (1,):
        raise AssertionError("assert_fk_rejected requires PRAGMA foreign_keys=ON")
    if db.in_transaction:
        raise AssertionError("assert_fk_rejected requires ownership of transaction state")
    baseline_violations = db.execute("PRAGMA foreign_key_check").fetchall()
    if baseline_violations:
        raise AssertionError(
            "assert_fk_rejected requires a clean foreign-key baseline; "
            f"found {baseline_violations!r}"
        )
    if not expected:
        raise AssertionError("assert_fk_rejected requires at least one expected FK")

    db.execute("BEGIN")
    db.execute("PRAGMA defer_foreign_keys=ON")
    try:
        try:
            invalid_operation(db)
        except Exception as error:
            raise AssertionError(
                "invalid operation failed before a deferred FK could be observed: "
                f"{type(error).__name__}: {error}"
            ) from error
        if not db.in_transaction:
            raise AssertionError("invalid operation ended the helper transaction")

        observed = _observed_foreign_keys(db)
        if observed != expected:
            raise AssertionError(
                "observed foreign keys did not equal expected: "
                f"expected={expected!r}, observed={observed!r}"
            )

        try:
            db.commit()
        except sqlite3.IntegrityError as error:
            error_code = getattr(error, "sqlite_errorcode", None)
            if error_code != sqlite3.SQLITE_CONSTRAINT_FOREIGNKEY:
                raise AssertionError(
                    "commit failed with the wrong SQLite constraint code: "
                    f"expected={sqlite3.SQLITE_CONSTRAINT_FOREIGNKEY}, "
                    f"observed={error_code!r}"
                ) from error
            error_name = getattr(error, "sqlite_errorname", None)
            if (
                error_name is not None
                and error_name != "SQLITE_CONSTRAINT_FOREIGNKEY"
            ):
                raise AssertionError(
                    "commit failed with the wrong SQLite constraint name: "
                    "expected='SQLITE_CONSTRAINT_FOREIGNKEY', "
                    f"observed={error_name!r}"
                ) from error
        except Exception as error:
            raise AssertionError(
                "commit failed with a non-FK exception: "
                f"{type(error).__name__}: {error}"
            ) from error
        else:
            raise AssertionError("invalid operation committed successfully")
    finally:
        if db.in_transaction:
            db.rollback()

    restored_violations = db.execute("PRAGMA foreign_key_check").fetchall()
    if restored_violations:
        raise AssertionError(
            "rollback did not restore the clean seed state: "
            f"{restored_violations!r}"
        )

    positive_changes = db.total_changes
    db.execute("BEGIN")
    db.execute("PRAGMA defer_foreign_keys=ON")
    try:
        positive_control(db)
        if not db.in_transaction:
            raise AssertionError("positive control ended the helper transaction")
        if db.total_changes == positive_changes:
            raise AssertionError("positive control did not write any rows")
        db.commit()
    except Exception as error:
        if db.in_transaction:
            db.rollback()
        if isinstance(error, AssertionError):
            raise
        raise AssertionError(
            "positive control did not write and commit successfully: "
            f"{type(error).__name__}: {error}"
        ) from error

    remaining_violations = db.execute("PRAGMA foreign_key_check").fetchall()
    if remaining_violations:
        raise AssertionError(
            "positive control left foreign-key violations: "
            f"{remaining_violations!r}"
        )
