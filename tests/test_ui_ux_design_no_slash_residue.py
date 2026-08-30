import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "ui-ux-design"


def test_ui_ux_entrypoints_have_no_retired_command_interface():
    corpus = "\n".join(
        path.read_text(errors="ignore")
        for path in (SKILL / "SKILL.md", SKILL / "agents" / "openai.yaml")
    )
    assert not re.search(
        r"\$ui-ux-design\s+(?:teach|document|polish|bolder|quieter|adapt|animate|live)\b",
        corpus,
        re.I,
    )
