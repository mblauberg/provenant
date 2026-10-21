#!/usr/bin/env python3
"""Shared review checks for the HARNESS.md Review ladder table.

Callers normalize their receipt-specific review records to legs with ``role``,
``family``, ``status``, ``lenses`` and, when applicable, ``reason``. Receipt
shape, independence, evidence and route checks remain owned by the caller; this
module counts passing reviews by family and lens, regardless of receipt role.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from typing import Any


PRIMARY_FAMILIES = frozenset({"openai", "anthropic"})
TERMINAL_PRESSURE_MARKERS = ("adversarial", "challenge")
REVIEW_PLAN_COMPLETE_STATUS = "complete"
REVIEW_PLAN_STATUSES = frozenset({
    REVIEW_PLAN_COMPLETE_STATUS,
    "failed",
    "unavailable",
    "omitted",
})
SKIPPED_STATUSES = REVIEW_PLAN_STATUSES - {REVIEW_PLAN_COMPLETE_STATUS}
# HARNESS.md permits a recorded provider blocker, never a failed review,
# when the required cross-family coverage is unavailable.
BLOCKER_STATUSES = frozenset({"omitted", "unavailable"})


def _lenses(leg: Mapping[str, Any]) -> set[str]:
    values = leg.get("lenses", ())
    if isinstance(values, str):
        values = (values,)
    if not isinstance(values, Iterable):
        return set()
    return {value.strip() for value in values if isinstance(value, str) and value.strip()}


def _provider_family(leg: Mapping[str, Any]) -> object:
    """Use validated endpoint lineage when the caller has retained it."""
    provider = leg.get("provider_family")
    family = provider if isinstance(provider, str) and provider else leg.get("family")
    return family.strip() if isinstance(family, str) else family


def _has_reason(leg: Mapping[str, Any]) -> bool:
    reason = leg.get("reason")
    return isinstance(reason, str) and bool(reason.strip())


def check_review_ladder(
    risk_tier: object,
    legs: Iterable[Mapping[str, Any]],
    *,
    chair_family: str | None = None,
) -> list[str]:
    """Return machine-checkable errors for the shared risk-scaled ladder.

    ``legs`` are deliberately small normalized records. A passing leg has
    ``status == 'pass'``; incomplete legs may carry ``reason``.
    """

    if risk_tier not in {"substantial", "crucial", "terminal"}:
        return []
    if not isinstance(chair_family, str) or not chair_family.strip():
        return ["chair_family is required"]

    checked = list(legs)
    passing = [leg for leg in checked if leg.get("status") == "pass" and _lenses(leg)]
    blocked_families = {
        family for leg in checked
        if leg.get("status") in BLOCKER_STATUSES and _has_reason(leg)
        and isinstance(family := _provider_family(leg), str)
        and family and family != chair_family
    }
    non_chair_families = {
        family for leg in passing
        if isinstance(family := _provider_family(leg), str)
        and family and chair_family and family != chair_family
    }
    errors: list[str] = []
    if risk_tier == "substantial":
        if not passing:
            errors.append("substantial review requires at least one passing independent leg")
        if not non_chair_families:
            if not blocked_families:
                errors.append("substantial review requires cross-family coverage or a recorded blocker with a reason")
            if not any(_provider_family(leg) == chair_family and _lenses(leg) for leg in passing):
                errors.append("substantial fallback requires a passing same-family review with a lens")
        return errors

    if len(passing) < 2:
        errors.append(f"{risk_tier} review requires at least two passing independent legs")
    lens_sets = [_lenses(leg) for leg in passing]
    if not any(
        left and right and left != right and len(left | right) >= 2
        for index, left in enumerate(lens_sets)
        for right in lens_sets[index + 1:]
    ):
        errors.append(f"{risk_tier} review requires distinct lens sets covering at least two lenses")
    if len(non_chair_families | blocked_families) < 2:
        errors.append(f"{risk_tier} review requires two non-chair families or a recorded blocker with a reason for each missing family")
    missing_families = max(0, 2 - len(non_chair_families))
    same_family_passes = sum(_provider_family(leg) == chair_family for leg in passing)
    if same_family_passes < missing_families:
        errors.append(f"{risk_tier} fallback requires a passing same-family review for each missing non-chair family")

    if risk_tier == "terminal":
        if not any(
            marker in lens.lower()
            for lenses in lens_sets for lens in lenses
            for marker in TERMINAL_PRESSURE_MARKERS
        ):
            errors.append("terminal review requires an adversarial or challenge lens")

    return errors
