from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SKILL_ROOT = ROOT / "skills" / "legal-writing"


def test_lint_uses_generic_safety_instrument_overstatement_rule() -> None:
    module_path = SKILL_ROOT / "scripts" / "lint_legal_style.py"
    spec = importlib.util.spec_from_file_location("legal_style_lint", module_path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)

    patterns = dict(module.FAIL_PATTERNS)
    safety_pattern = patterns["safety-instrument overstatement"]
    assert safety_pattern.search("The protection notice proves family violence.")
    assert not safety_pattern.search(
        "The reasons at paragraph 18 establish the finding relied on."
    )


def test_legal_lint_fails_closed_for_missing_or_empty_inputs(tmp_path) -> None:
    script = SKILL_ROOT / "scripts" / "lint_legal_style.py"
    missing = subprocess.run(
        [sys.executable, str(script), str(tmp_path / "missing")],
        capture_output=True,
        text=True,
    )
    assert missing.returncode == 1
    assert "FAIL missing path" in missing.stderr
    assert "FAIL no Markdown files resolved" in missing.stderr

    empty = tmp_path / "empty"
    empty.mkdir()
    rejected = subprocess.run(
        [sys.executable, str(script), str(empty)],
        capture_output=True,
        text=True,
    )
    assert rejected.returncode == 1
    assert "FAIL no Markdown files resolved" in rejected.stderr

    allowed = subprocess.run(
        [sys.executable, str(script), "--allow-empty", str(empty)],
        capture_output=True,
        text=True,
    )
    assert allowed.returncode == 0


def test_legal_lint_fails_closed_for_unreadable_directory(tmp_path) -> None:
    if os.name != "posix":
        return
    script = SKILL_ROOT / "scripts" / "lint_legal_style.py"
    unreadable = tmp_path / "unreadable"
    unreadable.mkdir()
    unreadable.chmod(0)
    try:
        result = subprocess.run(
            [sys.executable, str(script), "--allow-empty", str(unreadable)],
            capture_output=True,
            text=True,
        )
    finally:
        unreadable.chmod(0o700)
    assert result.returncode == 1
    assert "FAIL unreadable directory" in result.stderr
