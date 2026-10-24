from __future__ import annotations

from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[1]


def _text(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def test_academic_capability_moved_into_the_two_writing_owners() -> None:
    assert not (ROOT / "skills/academic-writing").exists()

    prose = ROOT / "skills/natural-writing/references/academic-prose.md"
    artefacts = ROOT / "skills/engineering-writing/references/academic-artefacts.md"
    assert prose.is_file() and artefacts.is_file()


def _description(skill: str) -> str:
    front = _text(f"skills/{skill}/SKILL.md").split("---", 2)[1]
    return yaml.safe_load(front)["description"]


def test_both_writing_owners_name_academic_prose_in_their_trigger() -> None:
    assert "academic" in _description("natural-writing").casefold()
    assert "thesis" in _description("natural-writing").casefold()
    assert "academic" in _description("engineering-writing").casefold()
    assert "thesis" in _description("engineering-writing").casefold()
