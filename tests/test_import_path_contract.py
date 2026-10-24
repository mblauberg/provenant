"""No file repairs `sys.path` for itself any more (issue #755)."""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]

# Both files run a probe under `python -I`, which ignores PYTHONPATH by design
# and, since it implies `-P`, also drops the script-directory entry a real
# invocation gets. Each probe's whole assertion is about the import root the
# caller names, so the caller's `sys.path` is the subject under test inside a
# quoted `-c` snippet, not a bootstrap in the test module itself.
EXEMPT: set[str] = set()


def _sources(directory: str):
    return sorted(
        path
        for path in (ROOT / directory).rglob("*.py")
        if str(path.relative_to(ROOT)) not in EXEMPT
    )


def _mutates_sys_path(path: Path) -> bool:
    tree = ast.parse(path.read_text())
    return any(
        isinstance(node, ast.Call)
        and isinstance(node.func, ast.Attribute)
        and node.func.attr in {"append", "insert"}
        and isinstance(node.func.value, ast.Attribute)
        and node.func.value.attr == "path"
        and isinstance(node.func.value.value, ast.Name)
        and node.func.value.value.id == "sys"
        for node in ast.walk(tree)
    )


@pytest.mark.parametrize(
    "directory",
    (
        pytest.param("tests", id="test_no_sys_path_mutation_under_tests"),
        pytest.param("scripts", id="test_no_sys_path_mutation_under_scripts"),
    ),
)
def test_no_sys_path_mutation(directory):
    # issue #755
    offenders = [
        str(path.relative_to(ROOT)) for path in _sources(directory) if _mutates_sys_path(path)
    ]
    assert offenders == []


def test_declared_import_roots_make_the_helpers_importable():
    from scripts.lib import roots  # noqa: F401  (product root)

    assert roots.product_root() == ROOT
