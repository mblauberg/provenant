"""Require fixture CHECK error constants to come from the shipped migration."""

from __future__ import annotations

import ast
from pathlib import Path
import re
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
UNVERIFIABLE_ERROR_BODY = "<value is not a static string>"
INLINE_ERROR_LABEL = "<inline>"

CheckAssertion = tuple[Path, int, str, str]


def normalise_sql(text: str) -> str:
    """Normalise SQL text so only token differences remain.

    Whitespace is collapsed and then removed around operators and punctuation,
    because the migration and the fixtures space these differently and a spacing
    difference is not a fabricated constraint. The same transform is applied to
    both sides, so the comparison stays honest about which tokens are present.
    """
    collapsed = " ".join(text.split())
    return re.sub(r"\s*(<=|>=|<>|!=|[=<>+\-*/,()])\s*", r"\1", collapsed)


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


def _error_constant_names(node: ast.AST) -> list[str]:
    """Return the ``*_ERROR`` names a statement assigns, if any."""
    if isinstance(node, ast.Assign):
        targets: list[ast.expr] = list(node.targets)
    elif isinstance(node, ast.AnnAssign):
        targets = [node.target]
    else:
        return []
    return [
        target.id
        for target in targets
        if isinstance(target, ast.Name) and target.id.endswith("_ERROR")
    ]


def _collect(node: ast.AST, fixture: Path, assertions: list[CheckAssertion]) -> None:
    """Record every CHECK assertion reachable from ``node``.

    A CHECK assertion is any string carrying the SQLite CHECK failure prefix,
    named or inline. An f-string carrying that prefix cannot be compared against
    the migration, so it is recorded as unverifiable and fails the gate rather
    than being skipped.
    """
    names = _error_constant_names(node)
    if names:
        assert isinstance(node, (ast.Assign, ast.AnnAssign))
        value = None if node.value is None else static_string(node.value)
        if value is None:
            for name in names:
                assertions.append((fixture, node.lineno, name, UNVERIFIABLE_ERROR_BODY))
            return
        if CHECK_ERROR_PREFIX in value:
            body = normalise_sql(value.split(CHECK_ERROR_PREFIX, 1)[1])
            for name in names:
                assertions.append((fixture, node.lineno, name, body))
        return

    if isinstance(node, ast.expr):
        value = static_string(node)
        if value is not None:
            if CHECK_ERROR_PREFIX in value:
                body = normalise_sql(value.split(CHECK_ERROR_PREFIX, 1)[1])
                assertions.append((fixture, node.lineno, INLINE_ERROR_LABEL, body))
            return
        if isinstance(node, ast.JoinedStr) and any(
            isinstance(part, ast.Constant)
            and isinstance(part.value, str)
            and CHECK_ERROR_PREFIX in part.value
            for part in node.values
        ):
            assertions.append(
                (fixture, node.lineno, INLINE_ERROR_LABEL, UNVERIFIABLE_ERROR_BODY)
            )
            return

    for child in ast.iter_child_nodes(node):
        _collect(child, fixture, assertions)


def check_assertions(fixtures: Path) -> list[CheckAssertion]:
    """Extract every CHECK message asserted by the fixtures, named or inline."""
    assertions: list[CheckAssertion] = []
    for fixture in sorted(fixtures.rglob("*.py")):
        tree = ast.parse(fixture.read_text(encoding="utf-8"), filename=str(fixture))
        _collect(tree, fixture, assertions)
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
                    # SQLite reports a named constraint by its name rather than
                    # by its body, so the name is an equally valid assertion.
                    named = re.search(
                        r"CONSTRAINT\s+([A-Za-z_][A-Za-z0-9_]*)\s*$",
                        sql[:index],
                        re.IGNORECASE,
                    )
                    if named is not None:
                        constraints.add(normalise_sql(named.group(1)))
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
