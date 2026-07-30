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
                'PREFIX = "CHECK constraint failed: "',
                'DYNAMIC_ERROR = PREFIX + "dynamic_missing=1"',
                'NOT_A_CHECK_ERROR = "UNIQUE constraint failed: sample.id"',
                'NOT_AN_ERROR_MESSAGE = "CHECK constraint failed: ignored=1"',
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

    assert CHECKER.missing_check_assertions(fixtures, migration) == [
        (
            fixtures / "sample.py",
            3,
            "FRAGMENT_ERROR",
            '(ordinal=1 AND kind="primary")',
        ),
        (
            fixtures / "sample.py",
            5,
            "BOGUS_ERROR",
            "missing_from_migration=1",
        ),
        (
            fixtures / "sample.py",
            6,
            "CONCAT_ERROR",
            "concatenated_missing=1",
        ),
        (
            fixtures / "sample.py",
            9,
            "DYNAMIC_ERROR",
            CHECKER.UNVERIFIABLE_ERROR_BODY,
        ),
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
