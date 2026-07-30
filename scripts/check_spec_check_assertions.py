#!/usr/bin/env python3
"""Require fixture CHECK error constants to come from the shipped migration."""

from __future__ import annotations

import ast
from pathlib import Path
import sqlite3
import sys


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "spec_fixtures"
MIGRATION = (
    ROOT
    / "runtime"
    / "agent-fabric"
    / "migrations"
    / "0001-current-baseline.sql"
)
CHECK_ERROR_PREFIX = "CHECK constraint failed:"
UNVERIFIABLE_ERROR_BODY = "<non-static *_ERROR value>"

CheckAssertion = tuple[Path, int, str, str]


def normalise_sql(text: str) -> str:
    """Collapse whitespace in SQL text for comparison."""
    return " ".join(text.split())


def static_string(node: ast.expr) -> str | None:
    """Evaluate literal strings and their static ``+`` concatenations."""
    if isinstance(node, ast.Constant) and isinstance(node.value, str):
        return node.value
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
        left = static_string(node.left)
        right = static_string(node.right)
        if left is not None and right is not None:
            return left + right
    return None


def check_assertions(fixtures: Path) -> list[CheckAssertion]:
    """Extract ``*_ERROR`` CHECK messages and flag uninspectable values."""
    assertions: list[CheckAssertion] = []
    for fixture in sorted(fixtures.rglob("*.py")):
        tree = ast.parse(fixture.read_text(encoding="utf-8"), filename=str(fixture))
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign):
                targets = node.targets
            elif isinstance(node, ast.AnnAssign):
                targets = [node.target]
            else:
                continue
            names = [
                target.id
                for target in targets
                if isinstance(target, ast.Name) and target.id.endswith("_ERROR")
            ]
            if not names:
                continue
            value = static_string(node.value)
            if value is None:
                for name in names:
                    assertions.append(
                        (fixture, node.lineno, name, UNVERIFIABLE_ERROR_BODY)
                    )
                continue
            if CHECK_ERROR_PREFIX not in value:
                continue
            body = normalise_sql(value.split(CHECK_ERROR_PREFIX, 1)[1])
            for name in names:
                assertions.append((fixture, node.lineno, name, body))
    return sorted(assertions)


def _parenthesised_body(sql: str, opening: int) -> tuple[str, int]:
    """Return one balanced parenthesised SQL body and its closing position."""
    depth = 1
    quote = ""
    index = opening + 1
    while index < len(sql):
        char = sql[index]
        following = sql[index + 1] if index + 1 < len(sql) else ""
        if quote:
            if char == quote:
                if following == quote:
                    index += 2
                    continue
                quote = ""
            index += 1
            continue
        if char in {"'", '"', "`"}:
            quote = char
        elif char == "[":
            quote = "]"
        elif char == "-" and following == "-":
            newline = sql.find("\n", index + 2)
            index = len(sql) if newline == -1 else newline + 1
            continue
        elif char == "/" and following == "*":
            closing = sql.find("*/", index + 2)
            if closing == -1:
                raise ValueError("unterminated SQL block comment")
            index = closing + 2
            continue
        elif char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
            if depth == 0:
                return sql[opening + 1 : index], index
        index += 1
    raise ValueError("unterminated CHECK constraint")


def complete_check_constraints(sql: str) -> set[str]:
    """Extract complete CHECK bodies, preserving SQL tokens modulo whitespace."""
    constraints: set[str] = set()
    quote = ""
    index = 0
    while index < len(sql):
        char = sql[index]
        following = sql[index + 1] if index + 1 < len(sql) else ""
        if quote:
            if char == quote:
                if following == quote:
                    index += 2
                    continue
                quote = ""
            index += 1
            continue
        if char in {"'", '"', "`"}:
            quote = char
            index += 1
            continue
        if char == "[":
            quote = "]"
            index += 1
            continue
        if char == "-" and following == "-":
            newline = sql.find("\n", index + 2)
            index = len(sql) if newline == -1 else newline + 1
            continue
        if char == "/" and following == "*":
            closing = sql.find("*/", index + 2)
            if closing == -1:
                raise ValueError("unterminated SQL block comment")
            index = closing + 2
            continue
        if sql[index : index + 5].upper() == "CHECK":
            before = sql[index - 1] if index else ""
            after = sql[index + 5] if index + 5 < len(sql) else ""
            if not (before.isalnum() or before == "_") and not (
                after.isalnum() or after == "_"
            ):
                opening = index + 5
                while opening < len(sql) and sql[opening].isspace():
                    opening += 1
                if opening < len(sql) and sql[opening] == "(":
                    body, closing = _parenthesised_body(sql, opening)
                    constraints.add(normalise_sql(body))
                    index = closing + 1
                    continue
        index += 1
    return constraints


def migration_check_constraints(migration: Path) -> set[str]:
    """Extract complete CHECKs from migration by executing it against SQLite."""
    database = sqlite3.connect(":memory:")
    try:
        database.executescript(migration.read_text(encoding="utf-8"))
        definitions = database.execute(
            "SELECT sql FROM sqlite_schema "
            "WHERE type='table' AND sql IS NOT NULL ORDER BY name"
        ).fetchall()
    finally:
        database.close()

    constraints: set[str] = set()
    for (definition,) in definitions:
        constraints.update(complete_check_constraints(definition))
    return constraints


def missing_check_assertions(
    fixtures: Path = FIXTURES,
    migration: Path = MIGRATION,
) -> list[CheckAssertion]:
    """Return fixture CHECK assertions absent from the shipped migration."""
    migration_constraints = migration_check_constraints(migration)
    return [
        assertion
        for assertion in check_assertions(fixtures)
        if assertion[3] not in migration_constraints
    ]


def main() -> int:
    assertions = check_assertions(FIXTURES)
    missing = missing_check_assertions()
    if missing:
        print("spec CHECK assertion gate failed:", file=sys.stderr)
        for fixture, line, name, body in missing:
            relative = fixture.relative_to(ROOT)
            print(
                f"  {relative}:{line}: {name}: "
                f"CHECK constraint not found in {MIGRATION.relative_to(ROOT)}: {body}",
                file=sys.stderr,
            )
        return 1

    print(
        f"spec CHECK assertion gate passed: "
        f"{len(assertions)} fixture CHECK assertions match the shipped migration"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
