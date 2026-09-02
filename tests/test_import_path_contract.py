"""No file repairs `sys.path` for itself any more (issue #755)."""

from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MUTATION = re.compile(r"sys\.path\.(insert|append)\b")

# `test_install_skills` runs a probe under `python -I`, which ignores
# PYTHONPATH by design. The probe's whole assertion is that the installed
# per-entry layout is a sufficient import root for a caller who names it, so
# the caller's `sys.path` is the subject under test, not a bootstrap.
EXEMPT = {"tests/test_install_skills.py"}


def _sources(directory: str):
    return sorted(
        path
        for path in (ROOT / directory).rglob("*.py")
        if str(path.relative_to(ROOT)) not in EXEMPT
    )


def test_no_sys_path_mutation_under_tests():
    offenders = [
        str(path.relative_to(ROOT)) for path in _sources("tests") if MUTATION.search(path.read_text())
    ]
    assert offenders == []


def test_no_sys_path_mutation_under_scripts():
    offenders = [
        str(path.relative_to(ROOT)) for path in _sources("scripts") if MUTATION.search(path.read_text())
    ]
    assert offenders == []


def test_declared_import_roots_make_the_helpers_importable():
    import instance_installation  # noqa: F401  (scripts/ root)
    import _shared.review_ladder  # noqa: F401  (skills/ root)
    from scripts.lib import roots  # noqa: F401  (product root)

    assert roots.product_root() == ROOT
