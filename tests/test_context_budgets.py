import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def words(path: Path):
    return len(re.findall(r"\b[\w'-]+\b", path.read_text()))


def test_every_skill_entrypoint_stays_within_progressive_disclosure_budget():
    oversized = {
        path.relative_to(ROOT).as_posix(): words(path)
        for path in (ROOT / "skills").glob("*/SKILL.md")
        if words(path) > 500
    }
    assert oversized == {}


def test_always_loaded_bootstraps_stay_compact():
    assert words(ROOT / "AGENTS.md") <= 250
    assert words(ROOT / "HARNESS.md") <= 750
