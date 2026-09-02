from collections import Counter
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parents[1]
EVALS = ROOT / "skills" / "orchestrate" / "evals"


def _cases() -> list[dict[str, object]]:
    return yaml.safe_load((EVALS / "trigger_cases.yaml").read_text())["cases"]


def test_herdr_routing_boundaries_rebalance_the_exact_nine_cases() -> None:
    cases = _cases()
    assert Counter(case["relation"] for case in cases) == {
        "positive": 3,
        "negative": 3,
        "boundary": 3,
    }

    herdr_positive = next(case for case in cases if case["id"] == "q155")
    assert "split" in herdr_positive["prompt"].lower()
    assert "herdr" in herdr_positive["prompt"].lower()
    assert herdr_positive["expected"] == {
        "primary_skill": "orchestrate",
        "companion_skills": [],
    }

    passive_negative = next(case for case in cases if case["id"] == "q157")
    assert "mentions herdr" in passive_negative["prompt"].lower()
    assert passive_negative["expected"] == {
        "primary_skill": None,
        "companion_skills": [],
    }

    answer_boundary = next(case for case in cases if case["id"] == "q161")
    prompt = answer_boundary["prompt"].lower()
    assert all(fragment in prompt for fragment in ("claude pane", "fabric", "answer-bearing"))
    assert answer_boundary["expected"] == {
        "primary_skill": "code-review",
        "companion_skills": ["orchestrate"],
    }
