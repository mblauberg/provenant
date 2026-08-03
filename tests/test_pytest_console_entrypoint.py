import shutil
import subprocess
from pathlib import Path

import pytest


ROOT = Path(__file__).resolve().parents[1]
AFFECTED_MODULES = (
    "tests/test_product_root_resolver.py",
    "tests/test_projection.py",
    "tests/test_public_release_check.py",
    "tests/test_review_panel.py",
)


def test_console_script_collects_the_affected_modules() -> None:
    pytest_executable = shutil.which("pytest")
    if pytest_executable is None:
        pytest.skip("pytest console script is not available on PATH")

    result = subprocess.run(
        [pytest_executable, "--collect-only", "-q", *AFFECTED_MODULES],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
    )

    assert result.returncode == 0, (
        "pytest console-script collection failed\n"
        f"stdout:\n{result.stdout}\n"
        f"stderr:\n{result.stderr}"
    )
