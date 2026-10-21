from __future__ import annotations

import pytest

from skills._shared.review_ladder import check_review_ladder


CHAIR = "openai"


def leg(family="anthropic", lenses=("correctness",), status="pass", **extra):
    return {"role": "targeted", "family": family, "status": status, "lenses": lenses, **extra}


@pytest.mark.parametrize("legs", [[], [leg(status="failed")]])
def test_routine_requires_no_review_legs(legs):
    assert check_review_ladder("routine", legs, chair_family=CHAIR) == []


@pytest.mark.parametrize("family", ["anthropic", "google"])
@pytest.mark.parametrize("role", ["targeted", "other-primary", "distinct-family"])
def test_substantial_accepts_one_cross_family_leg_regardless_of_role(family, role):
    assert check_review_ladder("substantial", [leg(family, role=role)], chair_family=CHAIR) == []


def test_substantial_requires_a_passing_independent_leg():
    assert check_review_ladder("substantial", [], chair_family=CHAIR)


@pytest.mark.parametrize("status", ["unavailable", "omitted"])
def test_substantial_accepts_reasoned_blocker_and_same_family_lens(status):
    legs = [leg(CHAIR), leg(status=status, reason="provider quota")]
    assert check_review_ladder("substantial", legs, chair_family=CHAIR) == []


@pytest.mark.parametrize("reason", [None, "", "   "])
def test_blocker_requires_non_empty_reason(reason):
    legs = [leg(CHAIR), leg(status="unavailable", reason=reason)]
    assert check_review_ladder("substantial", legs, chair_family=CHAIR)


@pytest.mark.parametrize("lenses", [[], [""], ["   "]])
def test_same_family_fallback_requires_a_lens(lenses):
    legs = [leg(CHAIR, lenses), leg(status="omitted", reason="provider outage")]
    assert check_review_ladder("substantial", legs, chair_family=CHAIR)


def test_same_family_review_requires_a_recorded_blocker():
    assert check_review_ladder("substantial", [leg(CHAIR)], chair_family=CHAIR)


def test_failed_leg_never_counts_as_review_or_blocker():
    failed = leg(status="failed", reason="review found defects")
    assert check_review_ladder("substantial", [failed], chair_family=CHAIR)
    assert check_review_ladder("substantial", [leg(CHAIR), failed], chair_family=CHAIR)
    assert check_review_ladder("crucial", [leg(), leg("google", ["security"], status="failed")], chair_family=CHAIR)


def test_cross_family_pass_does_not_need_an_unrelated_blocker():
    legs = [leg(), leg("google", status="unavailable", reason="")]
    assert check_review_ladder("substantial", legs, chair_family=CHAIR) == []


def test_family_uses_validated_provider_lineage():
    legs = [leg(provider_family=CHAIR)]
    assert check_review_ladder("substantial", legs, chair_family=CHAIR)


def test_crucial_accepts_two_non_chair_families_with_different_lens_sets():
    legs = [leg(lenses=["correctness", "security"]), leg("google", ["security"])]
    assert check_review_ladder("crucial", legs, chair_family=CHAIR) == []


def test_crucial_requires_two_legs_even_when_one_has_many_lenses():
    assert check_review_ladder("crucial", [leg(lenses=["correctness", "security"])], chair_family=CHAIR)


@pytest.mark.parametrize("lenses", [["correctness"], ["correctness", "security"]])
def test_crucial_rejects_duplicate_lens_sets(lenses):
    legs = [leg(lenses=lenses), leg("google", lenses)]
    assert check_review_ladder("crucial", legs, chair_family=CHAIR)


def test_crucial_requires_at_least_two_named_lenses():
    legs = [leg(lenses=[]), leg("google")]
    assert check_review_ladder("crucial", legs, chair_family=CHAIR)


@pytest.mark.parametrize("families", [(CHAIR, CHAIR), (CHAIR, "anthropic"), ("anthropic", "anthropic")])
def test_crucial_accepts_fewer_non_chair_families_only_with_a_blocker(families):
    legs = [leg(families[0]), leg(families[1], ["security"])]
    assert check_review_ladder("crucial", legs, chair_family=CHAIR)
    legs.append(leg("google", status="omitted", reason="provider unavailable"))
    assert check_review_ladder("crucial", legs, chair_family=CHAIR) == []


@pytest.mark.parametrize("status,reason", [("failed", "defects"), ("omitted", ""), ("unavailable", "   ")])
def test_crucial_rejects_invalid_blocker_records(status, reason):
    legs = [leg(), leg(CHAIR, ["security"]), leg("google", status=status, reason=reason)]
    assert check_review_ladder("crucial", legs, chair_family=CHAIR)


@pytest.mark.parametrize("lens", ["adversarial", "Challenge assumptions"])
def test_terminal_adds_adversarial_or_challenge_lens_without_requiring_three_lenses(lens):
    legs = [leg(), leg("google", [lens])]
    assert check_review_ladder("terminal", legs, chair_family=CHAIR) == []


def test_terminal_requires_challenge_from_a_passing_leg():
    legs = [leg(), leg("google", ["security"])]
    assert check_review_ladder("terminal", legs, chair_family=CHAIR)
    legs.append(leg(lenses=["adversarial"], status="failed"))
    assert check_review_ladder("terminal", legs, chair_family=CHAIR)


def test_terminal_still_requires_crucial_coverage():
    assert check_review_ladder("terminal", [leg(lenses=["correctness", "adversarial"])], chair_family=CHAIR)
