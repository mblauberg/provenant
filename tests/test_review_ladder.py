from __future__ import annotations

import pytest

from skills._shared.review_ladder import check_review_ladder


CHAIR = "openai"
S_PASS = "substantial review requires at least one passing independent leg"
S_FAMILY = "substantial review requires cross-family coverage or a recorded blocker with a reason"
S_FALLBACK = "substantial fallback requires a passing same-family review with a lens"
C_COUNT = "crucial review requires at least two passing independent legs"
C_LENSES = "crucial review requires distinct lens sets covering at least two lenses"
C_FAMILIES = "crucial review requires two non-chair families or a recorded blocker with a reason for each missing family"
C_FALLBACK = "crucial fallback requires a passing same-family review for each missing non-chair family"
T_COUNT = "terminal review requires at least two passing independent legs"
T_LENSES = "terminal review requires distinct lens sets covering at least two lenses"
T_FAMILIES = "terminal review requires two non-chair families or a recorded blocker with a reason for each missing family"
T_FALLBACK = "terminal fallback requires a passing same-family review for each missing non-chair family"
T_PRESSURE = "terminal review requires an adversarial or challenge lens"


def leg(family="anthropic", lenses=("correctness",), status="pass", **extra):
    return {"role": "targeted", "family": family, "status": status, "lenses": lenses, **extra}


@pytest.mark.parametrize("legs", [[], [leg(status="failed")]])
def test_routine_ladder_does_not_require_reviews(legs):
    assert check_review_ladder("routine", legs, chair_family=CHAIR) == []


@pytest.mark.parametrize("risk,chair", [
    ("substantial", None), ("substantial", ""), ("substantial", "   "),
    ("crucial", None), ("crucial", ""), ("crucial", "   "),
    ("terminal", None), ("terminal", ""), ("terminal", "   "),
])
def test_non_routine_ladders_require_a_chair_family(risk, chair):
    assert check_review_ladder(risk, [leg(), leg("google", ["security"])], chair_family=chair) == ["chair_family is required"]


@pytest.mark.parametrize("family,role", [
    ("anthropic", "targeted"), ("anthropic", "other-primary"), ("anthropic", "distinct-family"),
    ("google", "targeted"), ("google", "other-primary"), ("google", "distinct-family"),
])
def test_substantial_accepts_one_passing_cross_family_leg_in_any_role(family, role):
    assert check_review_ladder("substantial", [leg(family, role=role)], chair_family=CHAIR) == []


@pytest.mark.parametrize("legs,expected", [
    pytest.param([], [S_PASS, S_FAMILY, S_FALLBACK], id="no-review"),
    pytest.param([leg(lenses=[])], [S_PASS, S_FAMILY, S_FALLBACK], id="no-lens"),
    pytest.param([leg(CHAIR)], [S_FAMILY], id="same-family-needs-blocker"),
    pytest.param([leg(CHAIR, []), leg(status="omitted", reason="provider outage")], [S_PASS, S_FALLBACK], id="fallback-needs-lens"),
    pytest.param([leg(CHAIR), leg("   ", status="omitted", reason="provider unavailable")], [S_FAMILY], id="blank-family-cannot-block"),
    pytest.param([leg(CHAIR), leg(None, status="omitted", reason="provider unavailable")], [S_FAMILY], id="missing-family-cannot-block"),
    pytest.param([leg(CHAIR), leg("google", status="unavailable", reason="   ")], [S_FAMILY], id="blocker-needs-reason"),
    pytest.param([leg(status="failed", reason="review found defects")], [S_PASS, S_FAMILY, S_FALLBACK], id="failed-leg-is-not-a-review-or-blocker"),
    pytest.param([leg(), leg("google", status="unavailable", reason="")], [], id="invalid-unrelated-blocker-does-not-undo-pass"),
    pytest.param([leg(provider_family=CHAIR)], [S_FAMILY], id="validated-provider-lineage-wins"),
    pytest.param([leg("", provider_family="   ")], [S_FAMILY, S_FALLBACK], id="blank-provider-lineage-is-not-cross-family"),
])
def test_substantial_ladder_returns_the_expected_decision(legs, expected):
    assert check_review_ladder("substantial", legs, chair_family=CHAIR) == expected


@pytest.mark.parametrize("status", ["unavailable", "omitted"])
def test_substantial_same_family_fallback_requires_a_reasoned_blocker(status):
    assert check_review_ladder("substantial", [leg(CHAIR), leg(status=status, reason="provider quota")], chair_family=CHAIR) == []


@pytest.mark.parametrize("legs,expected", [
    pytest.param([leg(lenses=["correctness", "security"]), leg("google", ["security"])], [], id="two-families-different-lenses"),
    pytest.param([leg(lenses=["correctness", "security"])], [C_COUNT, C_LENSES, C_FAMILIES, C_FALLBACK], id="one-leg-cannot-cover-crucial"),
    pytest.param([leg(lenses=["correctness"]), leg("google", ["correctness"])], [C_LENSES], id="duplicate-lens-sets"),
    pytest.param([leg(lenses=[]), leg("google")], [C_COUNT, C_LENSES, C_FAMILIES, C_FALLBACK], id="unnamed-lenses-do-not-count"),
    pytest.param([leg(lenses=[]), leg("google", ["correctness", "security"])], [C_COUNT, C_LENSES, C_FAMILIES, C_FALLBACK], id="cross-family-pass-needs-named-lens"),
    pytest.param([leg(), leg(CHAIR, ["security"])], [C_FAMILIES], id="second-family-or-blocker-required"),
    pytest.param([leg(), leg(CHAIR, ["security"]), leg("google", status="omitted", reason="provider unavailable")], [], id="one-family-blocker-and-same-family-pass"),
    pytest.param([leg(), leg("google", ["security"], status="failed")], [C_COUNT, C_LENSES, C_FAMILIES, C_FALLBACK], id="failed-leg-does-not-count"),
    pytest.param([leg(), leg(CHAIR, ["security"]), leg("google", status="failed", reason="defects")], [C_FAMILIES], id="failed-review-is-not-a-blocker"),
    pytest.param([leg(), leg(CHAIR, ["security"]), leg("google", status="omitted", reason="")], [C_FAMILIES], id="omitted-blocker-needs-a-reason"),
    pytest.param([leg(), leg(CHAIR, ["security"]), leg("google", status="unavailable", reason="   ")], [C_FAMILIES], id="unavailable-blocker-needs-a-reason"),
    pytest.param([leg(), leg(lenses=["security"]), leg(status="omitted", reason="second route unavailable")], [C_FAMILIES, C_FALLBACK], id="blocker-from-passing-family-does-not-count-twice"),
    pytest.param([leg(), leg(lenses=["security"]), leg("google", status="omitted", reason="provider quota")], [C_FALLBACK], id="blocked-family-needs-same-family-replacement"),
])
def test_crucial_ladder_returns_the_expected_decision(legs, expected):
    assert check_review_ladder("crucial", legs, chair_family=CHAIR) == expected


@pytest.mark.parametrize("risk,blocked,expected", [
    pytest.param("crucial", ["google"], [C_FAMILIES], id="one-blocker"),
    pytest.param("crucial", ["google", "google"], [C_FAMILIES], id="duplicate-blocker-family"),
    pytest.param("crucial", ["google", CHAIR], [C_FAMILIES], id="chair-is-not-a-blocker"),
    pytest.param("crucial", ["google", "anthropic"], [], id="two-distinct-blockers"),
    pytest.param("terminal", ["google"], [T_FAMILIES], id="terminal-one-blocker"),
    pytest.param("terminal", ["google", "google"], [T_FAMILIES], id="terminal-duplicate-blocker-family"),
    pytest.param("terminal", ["google", CHAIR], [T_FAMILIES], id="terminal-chair-is-not-a-blocker"),
    pytest.param("terminal", ["google", "anthropic"], [], id="terminal-two-distinct-blockers"),
])
def test_same_family_exception_counts_distinct_non_chair_blockers(risk, blocked, expected):
    legs = [leg(CHAIR), leg(CHAIR, ["adversarial"])]
    legs.extend(leg(family, status="unavailable", reason="provider quota") for family in blocked)
    assert check_review_ladder(risk, legs, chair_family=CHAIR) == expected


@pytest.mark.parametrize("risk,legs,expected", [
    pytest.param("terminal", [leg(), leg("google", ["adversarial"])], [], id="adversarial-marker"),
    pytest.param("terminal", [leg(), leg("google", ["Challenge assumptions"])], [], id="challenge-marker"),
    pytest.param("terminal", [leg(), leg("google", ["security"]), leg(lenses=["adversarial"], status="failed")], [T_PRESSURE], id="failed-pressure-leg-does-not-count"),
    pytest.param("terminal", [leg(lenses=["correctness", "adversarial"])], [T_COUNT, T_LENSES, T_FAMILIES, T_FALLBACK], id="terminal-keeps-crucial-coverage"),
    pytest.param("terminal", [leg(lenses=[]), leg("google", ["correctness", "adversarial"])], [T_COUNT, T_LENSES, T_FAMILIES, T_FALLBACK], id="terminal-pass-needs-named-lens"),
])
def test_terminal_ladder_requires_passing_adversarial_pressure_and_crucial_coverage(risk, legs, expected):
    assert check_review_ladder(risk, legs, chair_family=CHAIR) == expected


def test_lens_sets_ignore_surrounding_whitespace():
    legs = [leg(lenses=["correctness"]), leg("google", [" correctness "])]
    assert check_review_ladder("crucial", legs, chair_family=CHAIR) == [C_LENSES]
