#!/usr/bin/env python3
"""Compare the SQL in docs/specs against the migration that actually ships.

`runtime/agent-fabric/migrations/0001-current-baseline.sql` is the sole schema
authority: `database-baseline.mjs` reads, executes and hashes only that file, and
`migrations.ts` rejects a hash mismatch. The hardening specifications carry a
second, hand-maintained copy of much of the same structure, and until this gate
nothing compared them. They drift on 44 of the 83 tables they share.

The migration side is not parsed. It is *executed* into an in-memory database and
read back through `PRAGMA table_info`, `index_list`, `index_info` and
`foreign_key_list`, so this half of the comparison is SQLite's own answer rather
than a regex approximation of it. Only the specification dialect — which is not
executable SQL — is parsed textually.

Scope, stated so the gate is not mistaken for more than it is: this compares
column *sets* and key-constraint signatures (PRIMARY KEY, UNIQUE, FOREIGN KEY)
per shared table. It does not compare types, NOT NULL, CHECK bodies, foreign-key
actions (`ON DELETE`/`ON UPDATE`), triggers or non-constraint indexes, and it
says nothing about whether the prose around the SQL is true.

Key signatures are column-order-sensitive: `UNIQUE(a,b)` and `UNIQUE(b,a)` are
distinct index prefixes and compare as drift.

A table named by a specification but absent from the schema is reported
separately rather than ignored, because an intersection-only comparison would
silently pass the worst case — a table that does not exist at all.

CHECK is deliberately excluded. Its body is free-form SQL that the typeless
specification dialect legitimately writes differently from the migration, so
comparing it would report drift on every table and mean nothing. The key
constraints have a canonical column-list form, so they compare honestly.

The gate is a ratchet. `spec-schema-drift-baseline.json` records the drift that
existed when it was introduced, so CI fails on *new* drift rather than on the
backlog. When a table is repaired the baseline entry must be removed in the same
change, which is enforced here — a stale allowance is itself a failure.
"""

from __future__ import annotations

from pathlib import Path
import json
import re
import sqlite3
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

# Specification DDL is typeless pseudo-SQL: "table_name(\n  col, col,\n)".
SPEC_TABLE = re.compile(r"\n([a-z_][a-z0-9_]*)\(\n(.*?)\n\)\n", re.S)
IDENTIFIER = re.compile(r"^([a-z_][a-z0-9_]*)")
# Key constraints, table-level and column-level.
TABLE_KEY = re.compile(r"^(PRIMARY\s+KEY|UNIQUE)\s*\(([^)]*)\)", re.I)
TABLE_FOREIGN_KEY = re.compile(
    r"^FOREIGN\s+KEY\s*\(([^)]*)\)\s*REFERENCES\s+(\w+)\s*(?:\(([^)]*)\))?", re.I
)
COLUMN_REFERENCES = re.compile(r"\bREFERENCES\s+(\w+)\s*(?:\(([^)]*)\))?", re.I)
NAMED_CONSTRAINT = re.compile(r"^CONSTRAINT\s+\w+\s+", re.I)
LINE_COMMENT = re.compile(r"--[^\n]*")

# A fragment that opens a table constraint rather than declaring a column. The
# trailing \b matters: a bare "CHECK" prefix also matches the column
# `checkpoint_id`, and a prefix test silently deleted 35 such columns from both
# sides of the comparison, where they agreed only by being equally invisible.
CONSTRAINT_OPENER = re.compile(
    r"(?:CHECK|PRIMARY\s+KEY|UNIQUE|FOREIGN\s+KEY|REFERENCES|CONSTRAINT"
    r"|DEFERRABLE|ON\s+(?:DELETE|UPDATE))\b",
    re.I,
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


def fragments(body: str) -> list[str]:
    """Split a body into whitespace-normalised fragments with comments removed.

    A trailing `-- comment` would otherwise be carried into the *next* fragment
    by the comma split and, once the newline is folded away, swallow the column
    declaration that follows it.
    """
    cleaned = []
    for fragment in split_top_level(body):
        text = " ".join(LINE_COMMENT.sub("", fragment).split())
        if text:
            cleaned.append(NAMED_CONSTRAINT.sub("", text))
    return cleaned


def opens_constraint(text: str) -> bool:
    """Whether a fragment declares a table constraint rather than a column."""
    match = CONSTRAINT_OPENER.match(text)
    return match is not None and match.start() == 0


def columns(body: str) -> tuple[set[str], bool]:
    """Return the declared column names, and whether the block was elided."""
    names: set[str] = set()
    elided = False
    for text in fragments(body):
        if ELISION in text:
            elided = True
            continue
        if opens_constraint(text):
            continue
        match = IDENTIFIER.match(text)
        if match:
            names.add(match.group(1))
    return names, elided


def normalise_columns(text: str) -> str:
    """Collapse a parenthesised column list to a canonical comma-joined form.

    Order is significant: `UNIQUE(a,b)` and `UNIQUE(b,a)` describe different
    index prefixes and compare as drift.
    """
    return ",".join(part.strip() for part in text.split(",") if part.strip())


def constraints(body: str) -> set[str]:
    """Return canonical PRIMARY KEY, UNIQUE and FOREIGN KEY signatures.

    Table and column spellings reduce to the same shape, so
    `admission_digest TEXT NOT NULL UNIQUE` and `UNIQUE(admission_digest)` are
    one signature, not two. This is the form SQLite's own PRAGMA output is
    rendered into, so the two sides of the comparison speak one language.
    """
    found: set[str] = set()
    for text in fragments(body):
        if ELISION in text:
            continue
        upper = text.upper()

        table_key = TABLE_KEY.match(text)
        if table_key:
            keyword = "PRIMARY KEY" if upper.startswith("PRIMARY") else "UNIQUE"
            found.add(f"{keyword}({normalise_columns(table_key.group(2))})")
            continue

        table_fk = TABLE_FOREIGN_KEY.match(text)
        if table_fk:
            source = normalise_columns(table_fk.group(1))
            # SQLite permits omitting the target list; the target's key is meant.
            target = normalise_columns(table_fk.group(3) or source)
            found.add(f"FOREIGN KEY({source})->{table_fk.group(2)}({target})")
            continue

        if opens_constraint(text):
            continue

        # A column declaration may carry the same constraints inline.
        column = IDENTIFIER.match(text)
        if not column:
            continue
        name = column.group(1)
        if re.search(r"\bPRIMARY\s+KEY\b", upper):
            found.add(f"PRIMARY KEY({name})")
        if re.search(r"\bUNIQUE\b", upper):
            found.add(f"UNIQUE({name})")
        column_fk = COLUMN_REFERENCES.search(text)
        if column_fk:
            target = normalise_columns(column_fk.group(2) or name)
            found.add(f"FOREIGN KEY({name})->{column_fk.group(1)}({target})")
    return found


Table = tuple[set[str], set[str]]


def load_migration() -> dict[str, Table]:
    """Execute the migration and read its shape back out of SQLite.

    Reading the schema through PRAGMA rather than a regex means the authoritative
    half of this comparison is SQLite's own answer. An earlier regex missed a
    whole table whose predecessor's body ended `));`, and silently folded it into
    the previous one; a parser cannot make that class of mistake if it is not
    doing the parsing.
    """
    database = sqlite3.connect(":memory:")
    database.executescript(MIGRATION.read_text(encoding="utf-8"))
    names = [
        row[0]
        for row in database.execute(
            "SELECT name FROM sqlite_master WHERE type='table' "
            "AND name NOT LIKE 'sqlite_%' ORDER BY name"
        )
    ]

    tables: dict[str, Table] = {}
    for table in names:
        info = database.execute(f'PRAGMA table_info("{table}")').fetchall()
        found = {row[1] for row in info}
        keys: set[str] = set()

        primary = [row[1] for row in sorted(info, key=lambda row: row[5]) if row[5]]
        if primary:
            keys.add(f"PRIMARY KEY({','.join(primary)})")

        for index in database.execute(f'PRAGMA index_list("{table}")'):
            # origin 'u' is a UNIQUE constraint; 'pk' is covered above and 'c' is
            # a standalone CREATE INDEX, which this gate does not compare.
            if index[3] != "u":
                continue
            members = database.execute(f'PRAGMA index_info("{index[1]}")').fetchall()
            ordered = [row[2] for row in sorted(members, key=lambda row: row[0])]
            keys.add(f"UNIQUE({','.join(ordered)})")

        references: dict[int, list[tuple[int, str, str, str]]] = {}
        for row in database.execute(f'PRAGMA foreign_key_list("{table}")'):
            references.setdefault(row[0], []).append((row[1], row[2], row[3], row[4]))
        for parts in references.values():
            parts.sort()
            target = parts[0][1]
            source_columns = ",".join(part[2] for part in parts)
            # A null target column means the reference named no column list, so
            # SQLite resolves it to the target's primary key.
            target_columns = ",".join(part[3] or part[2] for part in parts)
            keys.add(f"FOREIGN KEY({source_columns})->{target}({target_columns})")

        tables[table] = (found, keys)
    return tables


def load_tables() -> tuple[dict[str, Table], dict[str, Table], list[str]]:
    migration = load_migration()

    spec: dict[str, Table] = {}
    elisions: list[str] = []
    for name in SPEC_FILES:
        path = SPEC_DIR / name
        text = path.read_text(encoding="utf-8")
        for match in SPEC_TABLE.finditer(text):
            table = match.group(1)
            found, elided = columns(match.group(2))
            if elided:
                elisions.append(f"{path.relative_to(ROOT)}: {table} has an elided DDL block")
            spec[table] = (found, constraints(match.group(2)))
    return spec, migration, elisions


def measure() -> tuple[dict[str, dict[str, list[str]]], list[str], list[str], int]:
    """Return drift, elisions, specified-but-absent tables, and the shared count."""
    spec, migration, elisions = load_tables()
    drift: dict[str, dict[str, list[str]]] = {}
    shared = sorted(set(spec) & set(migration))
    for table in shared:
        spec_columns, spec_keys = spec[table]
        migration_columns, migration_keys = migration[table]
        entry = {
            "spec_only": sorted(spec_columns - migration_columns),
            "migration_only": sorted(migration_columns - spec_columns),
            "spec_only_keys": sorted(spec_keys - migration_keys),
            "migration_only_keys": sorted(migration_keys - spec_keys),
        }
        if any(entry.values()):
            drift[table] = entry
    return drift, sorted(elisions), sorted(set(spec) - set(migration)), len(shared)


def load_baseline() -> dict:
    if not BASELINE.is_file():
        return {"drift": {}, "elided": [], "absent": []}
    return json.loads(BASELINE.read_text(encoding="utf-8"))


def failures() -> list[str]:
    problems: list[str] = []
    drift, elisions, absent, _ = measure()
    baseline = load_baseline()
    allowed = baseline.get("drift", {})
    allowed_elisions = set(baseline.get("elided", []))
    allowed_absent = set(baseline.get("absent", []))

    for table in absent:
        if table not in allowed_absent:
            problems.append(
                f"{table} is declared in a specification but does not exist in the "
                f"migration under that name; check for a rename before recording it"
            )

    for stale in sorted(allowed_absent - set(absent)):
        problems.append(
            f"{stale} now exists in the migration but is still listed as absent in "
            f"{BASELINE.name}; remove the entry so the ratchet holds"
        )

    for table, entry in drift.items():
        if table not in allowed:
            problems.append(
                f"new drift in {table}: "
                f"{len(entry['spec_only'])} spec-only and "
                f"{len(entry['migration_only'])} migration-only column(s), "
                f"{len(entry['spec_only_keys'])} spec-only and "
                f"{len(entry['migration_only_keys'])} migration-only key(s); "
                f"spec-only={entry['spec_only'] or '-'} "
                f"migration-only={entry['migration_only'] or '-'} "
                f"spec-only-keys={entry['spec_only_keys'] or '-'} "
                f"migration-only-keys={entry['migration_only_keys'] or '-'}"
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
    drift, elisions, absent, shared = measure()

    if "--write-baseline" in sys.argv:
        payload = {"drift": drift, "elided": elisions, "absent": absent}
        BASELINE.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        print(
            f"spec-schema-drift: wrote {BASELINE.name} with {len(drift)} drifting "
            f"table(s), {len(elisions)} elided block(s) and {len(absent)} absent table(s)"
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
        f"{len(elisions)} elided, {len(absent)} specified but absent)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
