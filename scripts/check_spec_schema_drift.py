#!/usr/bin/env python3
"""Compare the SQL in docs/specs against the migration that actually ships.

`runtime/agent-fabric/migrations/0001-current-baseline.sql` is the sole schema
authority: `database-baseline.mjs` reads, executes and hashes only that file, and
`migrations.ts` rejects a hash mismatch. The hardening specifications carry a
second, hand-maintained copy of much of the same structure, and until this gate
nothing compared them. They had drifted on 19 of the 86 tables they share.

Scope, stated so the gate is not mistaken for more than it is: this compares
column *sets* per shared table. It does not compare types, constraints, triggers
or indexes, and it says nothing about whether the prose around the SQL is true.

The gate is a ratchet. `spec-schema-drift-baseline.json` records the drift that
existed when it was introduced, so CI fails on *new* drift rather than on the
backlog. When a table is repaired the baseline entry must be removed in the same
change, which is enforced here — a stale allowance is itself a failure.
"""

from __future__ import annotations

from pathlib import Path
import json
import re
import sys


ROOT = Path(__file__).resolve().parents[1]
MIGRATION = ROOT / "runtime" / "agent-fabric" / "migrations" / "0001-current-baseline.sql"
SPEC_DIR = ROOT / "docs" / "specs" / "agent-fabric"
BASELINE = ROOT / "scripts" / "spec-schema-drift-baseline.json"

# The SQL-bearing specifications. This is the same set the test fixtures treat as
# the hardening corpus; keep the two in step.
SPEC_FILES = (
    "architecture-assurance.md",
    "daemon-and-wire.md",
    "workspace-containment.md",
    "provider-custody.md",
    "review-custody.md",
    "persistence.md",
    "retention-and-exports.md",
    "observability.md",
    "recovery.md",
)

MIGRATION_TABLE = re.compile(r"CREATE TABLE (\w+)\s*\((.*?)\n\)\s*(?:STRICT)?\s*;", re.S)
# Specification DDL is typeless pseudo-SQL: "table_name(\n  col, col,\n)".
SPEC_TABLE = re.compile(r"\n([a-z_][a-z0-9_]*)\(\n(.*?)\n\)\n", re.S)
IDENTIFIER = re.compile(r"^([a-z_][a-z0-9_]*)")

# Fragments that open a table constraint rather than declare a column.
CONSTRAINTS = (
    "CHECK",
    "PRIMARY",
    "UNIQUE",
    "FOREIGN",
    "REFERENCES",
    "CONSTRAINT",
    "DEFERRABLE",
    "ON ",
)
# A prose elision inside a DDL block makes that block unparseable as SQL. It is
# reported rather than skipped, because silently ignoring it would let a spec
# opt out of this gate by writing "...".
ELISION = "..."


def split_top_level(body: str) -> list[str]:
    """Split a DDL body on the commas that sit at parenthesis depth zero."""
    fragments: list[str] = []
    depth = 0
    current: list[str] = []
    for char in body:
        if char == "(":
            depth += 1
        elif char == ")":
            depth -= 1
        if char == "," and depth == 0:
            fragments.append("".join(current))
            current = []
        else:
            current.append(char)
    fragments.append("".join(current))
    return fragments


def columns(body: str) -> tuple[set[str], bool]:
    """Return the declared column names, and whether the block was elided."""
    names: set[str] = set()
    elided = False
    for fragment in split_top_level(body):
        text = fragment.strip()
        if not text:
            continue
        if ELISION in text:
            elided = True
            continue
        if text.upper().startswith(CONSTRAINTS):
            continue
        match = IDENTIFIER.match(text)
        if match:
            names.add(match.group(1))
    return names, elided


def load_tables() -> tuple[dict[str, set[str]], dict[str, set[str]], list[str]]:
    migration_sql = MIGRATION.read_text(encoding="utf-8")
    migration = {
        match.group(1): columns(match.group(2))[0]
        for match in MIGRATION_TABLE.finditer(migration_sql)
    }

    spec: dict[str, set[str]] = {}
    elisions: list[str] = []
    for name in SPEC_FILES:
        path = SPEC_DIR / name
        text = path.read_text(encoding="utf-8")
        for match in SPEC_TABLE.finditer(text):
            table = match.group(1)
            found, elided = columns(match.group(2))
            if elided:
                elisions.append(f"{path.relative_to(ROOT)}: {table} has an elided DDL block")
            spec[table] = found
    return spec, migration, elisions


def measure() -> tuple[dict[str, dict[str, list[str]]], list[str], int]:
    """Return current drift, current elisions, and the shared-table count."""
    spec, migration, elisions = load_tables()
    drift: dict[str, dict[str, list[str]]] = {}
    shared = sorted(set(spec) & set(migration))
    for table in shared:
        spec_only = sorted(spec[table] - migration[table])
        migration_only = sorted(migration[table] - spec[table])
        if spec_only or migration_only:
            drift[table] = {"spec_only": spec_only, "migration_only": migration_only}
    return drift, sorted(elisions), len(shared)


def load_baseline() -> dict:
    if not BASELINE.is_file():
        return {"drift": {}, "elided": []}
    return json.loads(BASELINE.read_text(encoding="utf-8"))


def failures() -> list[str]:
    problems: list[str] = []
    drift, elisions, _ = measure()
    baseline = load_baseline()
    allowed = baseline.get("drift", {})
    allowed_elisions = set(baseline.get("elided", []))

    for table, entry in drift.items():
        if table not in allowed:
            problems.append(
                f"new drift in {table}: {len(entry['spec_only'])} spec-only, "
                f"{len(entry['migration_only'])} migration-only column(s); "
                f"spec-only={entry['spec_only'] or '-'} "
                f"migration-only={entry['migration_only'] or '-'}"
            )
        elif allowed[table] != entry:
            problems.append(
                f"drift in {table} changed since the baseline was recorded; "
                f"update {BASELINE.name} in this change"
            )

    for table in sorted(set(allowed) - set(drift)):
        problems.append(
            f"{table} no longer drifts but is still listed in {BASELINE.name}; "
            f"remove the entry so the ratchet holds"
        )

    for elision in elisions:
        if elision not in allowed_elisions:
            problems.append(f"new elided DDL block, which cannot be checked: {elision}")

    for stale in sorted(allowed_elisions - set(elisions)):
        problems.append(
            f"elision is gone but still listed in {BASELINE.name}: {stale}; "
            f"remove the entry so the ratchet holds"
        )

    return problems


def main() -> int:
    drift, elisions, shared = measure()

    if "--write-baseline" in sys.argv:
        payload = {"drift": drift, "elided": elisions}
        BASELINE.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(
            f"spec-schema-drift: wrote {BASELINE.name} with {len(drift)} drifting "
            f"table(s) and {len(elisions)} elided block(s)"
        )
        return 0

    problems = failures()
    if problems:
        for problem in problems:
            print(f"spec-schema-drift: {problem}", file=sys.stderr)
        return 1

    print(
        f"spec-schema-drift: ok ({shared} shared tables, "
        f"{shared - len(drift)} agree exactly, {len(drift)} known-drifting, "
        f"{len(elisions)} elided)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
