from __future__ import annotations

from skills._shared.review_ladder import check_review_ladder


CHAIR = "openai"


def targeted(family: str, lenses: list[str]) -> dict[str, object]:
    return {"role": "targeted", "family": family, "status": "pass", "lenses": lenses}


def other_primary_skip(reason: str) -> dict[str, object]:
    return {
        "role": "other-primary", "family": "anthropic", "status": "unavailable",
        "lenses": ["correctness"], "reason": reason,
    }


def test_skipped_other_primary_with_reason_is_covered_by_same_family_distinct_lens():
    legs = [
        targeted("anthropic", ["correctness", "tests"]),
        targeted(CHAIR, ["adversarial"]),
        other_primary_skip("provider-unavailable"),
    ]
    assert check_review_ladder("substantial", legs, chair_family=CHAIR) == []


def test_other_primary_skip_without_reason_is_rejected():
    legs = [
        targeted("anthropic", ["correctness", "tests"]),
        targeted(CHAIR, ["adversarial"]),
        other_primary_skip(""),
    ]
    errors = check_review_ladder("substantial", legs, chair_family=CHAIR)
    assert errors == ["other-primary skip requires a recorded reason"]


def test_other_primary_skip_without_covering_same_family_leg_is_rejected():
    legs = [
        targeted("anthropic", ["correctness", "tests"]),
        targeted("google", ["adversarial"]),
        other_primary_skip("provider-unavailable"),
    ]
    errors = check_review_ladder("substantial", legs, chair_family=CHAIR)
    assert errors == [
        "other-primary skip requires a passing same-family targeted review with a distinct lens"
    ]


def test_overlapping_review_does_not_invalidate_fallback_coverage():
    legs = [
        targeted(CHAIR, ["correctness"]),
        targeted(CHAIR, ["adversarial"]),
        targeted("anthropic", ["correctness", "adversarial"]),
        other_primary_skip("provider-unavailable"),
    ]
    assert check_review_ladder("substantial", legs, chair_family=CHAIR) == []


def test_duplicate_same_family_review_does_not_cover_the_skip():
    legs = [
        targeted("anthropic", ["correctness", "tests"]),
        targeted(CHAIR, ["correctness", "tests"]),
        other_primary_skip("provider-unavailable"),
    ]
    errors = check_review_ladder("substantial", legs, chair_family=CHAIR)
    assert errors == [
        "other-primary skip requires a passing same-family targeted review with a distinct lens"
    ]
