from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text()


def test_harness_python_selection_is_portable():
    checker = read("scripts/check-harness")

    assert "miniforge" not in checker
    assert 'requested_product_root="${AGENT_FABRIC_PRODUCT_ROOT:-' in checker
    assert 'PRODUCT_ROOT="$PWD"' in checker
    assert 'SCRIPTS_ROOT="${PROVENANT_SCRIPTS_ROOT:-' in checker
    assert 'SKILLS_ROOT="${PROVENANT_SKILLS_ROOT:-' in checker
    assert 'source "$SCRIPTS_ROOT/lib/harness-python.sh"' in checker
    assert 'run_test "$SCRIPTS_ROOT/check_harness.py"' in checker
    assert "command -v python3" not in checker
    assert "uv run --frozen --only-group test python" not in checker
    assert '"${PYTHON[@]}"' not in checker
