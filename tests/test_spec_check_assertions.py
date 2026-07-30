"""Regression coverage for fixture CHECK assertion provenance."""

from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "check_spec_check_assertions.py"
SPEC = spec_from_file_location("spec_check_assertions", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
CHECKER = module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


def test_missing_check_assertions_reports_only_unshipped_check_errors(
    tmp_path: Path,
) -> None:
    fixtures = tmp_path / "fixtures"
    fixtures.mkdir()
    (fixtures / "sample.py").write_text(
        "\n".join(
            (
                'MATCHED_ERROR = """CHECK constraint failed: (ordinal=1 AND',
                '  kind="primary") OR (ordinal=2 AND kind="secondary")"""',
                'FRAGMENT_ERROR = """CHECK constraint failed: (ordinal=1 AND',
                '  kind="primary")"""',
                'BOGUS_ERROR = "CHECK constraint failed: missing_from_migration=1"',
                'CONCAT_ERROR = ("CHECK constraint "',
                '  + "failed: concatenated_missing=1")',
                'DYNAMIC_ERROR = PREFIX + "dynamic_missing=1"',
                'NOT_A_CHECK_ERROR = "UNIQUE constraint failed: sample.id"',
                'reject(sql, message="CHECK constraint failed: inline_missing=1")',
                'reject(sql, message=f"CHECK constraint failed: {computed}")',
                'reject(sql, message="CHECK constraint failed: "',
                '  "(ordinal=1 AND kind=\\"primary\\") OR "',
                '  "(ordinal=2 AND kind=\\"secondary\\")")',
            )
        ),
        encoding="utf-8",
    )
    migration = tmp_path / "migration.sql"
    migration.write_text(
        'CREATE TABLE sample(ordinal INTEGER, kind TEXT, '
        'CHECK((ordinal=1 AND kind="primary") OR '
        '(ordinal=2 AND kind="secondary")));',
        encoding="utf-8",
    )

    sample = fixtures / "sample.py"
    assert CHECKER.missing_check_assertions(fixtures, migration) == [
        (sample, 3, "FRAGMENT_ERROR", '(ordinal=1 AND kind="primary")'),
        (sample, 5, "BOGUS_ERROR", "missing_from_migration=1"),
        (sample, 6, "CONCAT_ERROR", "concatenated_missing=1"),
        (sample, 8, "DYNAMIC_ERROR", CHECKER.UNVERIFIABLE_ERROR_BODY),
        (sample, 10, CHECKER.INLINE_ERROR_LABEL, "inline_missing=1"),
        (sample, 11, CHECKER.INLINE_ERROR_LABEL, CHECKER.UNVERIFIABLE_ERROR_BODY),
    ]
    # The inline assertion spanning lines 12 to 14 matches the migration and so is
    # absent above, which is what stops the gate from failing on real assertions.
    assert len(CHECKER.check_assertions(fixtures)) == 8


def test_gate_normalises_spacing_and_accepts_named_constraints(tmp_path: Path) -> None:
    """A spacing difference is not a fabricated constraint, and a named CHECK is
    reported by SQLite under its name rather than its body."""
    fixtures = tmp_path / "fixtures"
    fixtures.mkdir()
    (fixtures / "sample.py").write_text(
        "\n".join(
            (
                'SPACED_ERROR = "CHECK constraint failed: ordinal=prior_ordinal+1"',
                'NAMED_ERROR = "CHECK constraint failed: sample_ordinal_guard"',
                'WRONG_NAME_ERROR = "CHECK constraint failed: no_such_guard"',
            )
        ),
        encoding="utf-8",
    )
    migration = tmp_path / "migration.sql"
    migration.write_text(
        "CREATE TABLE sample(\n"
        "  ordinal INTEGER,\n"
        "  prior_ordinal INTEGER,\n"
        "  CHECK (ordinal = prior_ordinal + 1),\n"
        "  CONSTRAINT sample_ordinal_guard CHECK (ordinal >= 0)\n"
        ");",
        encoding="utf-8",
    )

    assert CHECKER.missing_check_assertions(fixtures, migration) == [
        (fixtures / "sample.py", 3, "WRONG_NAME_ERROR", "no_such_guard"),
    ]


def test_current_fixture_check_assertions_exist_in_shipped_migration() -> None:
    assert CHECKER.missing_check_assertions(
        ROOT / "tests" / "spec_fixtures",
        ROOT
        / "runtime"
        / "agent-fabric"
        / "migrations"
        / "0001-current-baseline.sql",
    ) == []
