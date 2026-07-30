from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def read(path: str) -> str:
    return (ROOT / path).read_text()


def test_repair_budget_has_one_enforced_limit():
    architecture = read("docs/ARCHITECTURE.md")

    assert "up to 5 for complex work" not in architecture
    assert "routine 2 cycles, substantial 4, and crucial and terminal 5" in architecture
    assert "at most 2 repair cycles" not in architecture


def test_harness_python_selection_is_portable():
    checker = read("scripts/check-harness")

    assert "miniforge" not in checker
    assert '"$ROOT/.venv/bin/python"' in checker
    assert "command -v python3" in checker
    assert "import pytest, yaml" in checker
    assert "uv run --frozen --only-group test python" in checker
    assert '"${PYTHON[@]}"' in checker
    assert '"$PYTHON"' not in checker


def test_install_and_continuity_docs_describe_the_actual_boundaries():
    readme = read("README.md")
    session = read("skills/session/SKILL.md")

    assert "persist it in the shell rc" not in readme
    assert "command collision, incompatible instruction target, or managed skill-link conflict" in " ".join(readme.split())
    assert "Project instructions may override continuity paths." in " ".join(session.split())
