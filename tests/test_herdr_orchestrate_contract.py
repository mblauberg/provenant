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


def test_herdr_reference_and_degradation_doctrines_are_contract_invariants() -> None:
    manifest = yaml.safe_load(
        (ROOT / "tests" / "fixtures" / "disclosure-migration.yaml").read_text()
    )
    required_refs = {
        row["file"]
        for row in manifest["orchestrate"]
        if row["verdict"] in {"keep", "slim"}
    }
    contract = yaml.safe_load((EVALS / "contract_cases.yaml").read_text())
    invariants = set(contract["reference_invariants"])

    assert "herdr-panes.md" in required_refs
    assert {
        "herdr-panes.md",
        "HERDR-NOT-USED",
        "dispatched-unconfirmed",
        "referenceValidation: verified",
        "FABRIC-ROUNDTRIP-UNAVAILABLE",
        "Herdr only observes or sends fire-and-forget steering",
        "send a correlated reply, then explicitly acknowledge",
        "claim TTL must cover expected processing and artifact custody",
        "dedupes by the request `reply_to` together with the named artifact and digest",
    } <= invariants


def test_reply_precedes_ack_and_duplicate_redelivery_is_deduped() -> None:
    for relative in (
        "skills/orchestrate/references/herdr-panes.md",
        "skills/orchestrate/references/paired-primary.md",
    ):
        source = (ROOT / relative).read_text()
        normalized = " ".join(source.split())
        assert "reply, then" in normalized
        assert "claim TTL must cover expected processing and artifact custody" in normalized
        assert "there is no renewal" in normalized or "it has no renewal" in normalized
        assert "dedupes by the request `reply_to`" in normalized
        assert "acknowledges the claim after durable processing" not in source


def test_worktree_recipient_failure_is_explicit_until_the_label_is_announced() -> None:
    source = (ROOT / "skills" / "orchestrate" / "references" / "herdr-panes.md").read_text()
    assert "unknown-recipient error" in source
    assert "does not silently land in the" in source
    assert "simply lands in a project the" not in source


def test_reader_facing_herdr_descriptions_do_not_promise_wake_or_callback() -> None:
    for path in (ROOT / "README.md", ROOT / "docs" / "ARCHITECTURE.md"):
        source = path.read_text()
        assert "observes and wakes" not in source
        assert "wake signals" not in source
    assert "fire-and-forget\nsteering" in (ROOT / "README.md").read_text()
