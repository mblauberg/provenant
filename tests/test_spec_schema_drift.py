"""Regression coverage for specification foreign-key target accounting."""

from importlib.util import module_from_spec, spec_from_file_location
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts" / "check_spec_schema_drift.py"
SPEC = spec_from_file_location("spec_schema_drift", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
CHECKER = module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


def test_foreign_key_targets_include_inline_and_table_constraints() -> None:
    body = """
      parent_id TEXT REFERENCES inline_parent(parent_id),
      FOREIGN KEY (project_id, run_id)
        REFERENCES composite_parent(project_id, run_id)
    """

    assert CHECKER.foreign_key_targets(body) == {
        "composite_parent",
        "inline_parent",
    }


def test_current_specification_references_are_declared() -> None:
    _, _, _, dangling, _ = CHECKER.measure()

    assert dangling == []


def test_undeclared_foreign_key_target_is_dangling() -> None:
    dangling = CHECKER.dangling_foreign_key_targets(
        {"declared_in_spec", "declared_in_migration", "missing_target"},
        {"declared_in_spec"},
        {"declared_in_migration": (set(), set())},
    )

    assert dangling == {"missing_target"}
