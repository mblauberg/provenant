"""Contract tests for the CLI's recorded, language-independent cases."""

import os
from pathlib import Path
import sys

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run import binary_path, compare  # noqa: E402


def test_golden_cli_cases_match():
    binary = binary_path()
    if binary is None or (isinstance(binary, Path) and not binary.is_file()):
        reason = "no CLI binary found (set PROVENANT_GOLDEN_BIN or build tools/front-door)"
        if os.environ.get("PROVENANT_GOLDEN_REQUIRED") == "1":
            pytest.fail(reason)
        pytest.skip(reason)
    assert compare(binary) == 0
