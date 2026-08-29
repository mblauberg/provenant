import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "ui-ux-design"


def test_shipped_ui_ux_contract_has_no_retired_command_interface():
    corpus = "\n".join(
        path.read_text(errors="ignore")
        for path in sorted(SKILL.rglob("*"))
        if path.is_file() and path.suffix in {".md", ".yaml", ".js", ".mjs"}
    )
    assert not re.search(r"(?<!skills)/(?:ui-ux-design|frontend-design)(?:\s|$)", corpus)
    assert not re.search(
        r"\$ui-ux-design\s+(?:teach|document|polish|bolder|quieter|adapt|animate|live)\b",
        corpus,
        re.I,
    )
    for pseudo in ("known first word", "grouped menu", "command catalogue", "command routing"):
        assert pseudo not in corpus.lower()
    assert not re.search(r"ui-ux-design\s+(?:teach|document|polish|bolder|adapt)\b", corpus, re.I)
