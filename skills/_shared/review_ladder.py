#!/usr/bin/env python3
"""Shared review-pressure ladder checks for the harness validators.

Callers normalize their receipt-specific review records to legs with ``role``,
``family``, ``status``, ``lenses`` and, when applicable, ``reason``. Receipt
shape, evidence and route checks remain owned by
the caller; this module owns only the risk-scaled review contract in HARNESS.md.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from typing import Any


PRIMARY_FAMILIES = frozenset({"openai", "anthropic"})
TARGETED_LENS_MINIMUM = 2
TERMINAL_TARGETED_LENS_MINIMUM = 3
TERMINAL_PRESSURE_MARKERS = ("adversarial", "challenge")
REVIEW_PLAN_COMPLETE_STATUS = "complete"
REVIEW_PLAN_STATUSES = frozenset({
    REVIEW_PLAN_COMPLETE_STATUS,
    "failed",
    "unavailable",
    "omitted",
})
SKIPPED_STATUSES = REVIEW_PLAN_STATUSES - {REVIEW_PLAN_COMPLETE_STATUS}
# HARNESS.md: a skipped (omitted) or unavailable other-primary leg may be
# covered by same-family review, but a failed leg never is.
OTHER_PRIMARY_FALLBACK_STATUSES = frozenset({"omitted", "unavailable"})


def _lenses(leg: Mapping[str, Any]) -> set[str]:
    values = leg.get("lenses", ())
    if isinstance(values, str):
        values = (values,)
    if not isinstance(values, Iterable):
        return set()
    return {value for value in values if isinstance(value, str) and value}


def _provider_family(leg: Mapping[str, Any]) -> object:
    """Use validated endpoint lineage when the caller has retained it."""
    provider = leg.get("provider_family")
    return provider if isinstance(provider, str) and provider else leg.get("family")


def _has_reason(leg: Mapping[str, Any]) -> bool:
    reason = leg.get("reason")
    return isinstance(reason, str) and bool(reason.strip())


def _covers_other_primary(checked: list[Mapping[str, Any]], chair_family: str | None) -> bool:
    """A passing same-family targeted leg whose lenses differ from each other passing targeted leg's.

    Overlap is allowed; only a duplicate of another review's lenses does not count.
    """
    if not chair_family:
        return False
    targeted = [leg for leg in checked if leg.get("role") == "targeted" and leg.get("status") == "pass"]
    return any(
        _provider_family(leg) == chair_family
        and all(_lenses(leg) != _lenses(other) for other in targeted if other is not leg)
        for leg in targeted
    )


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

    checked = list(legs)
    errors: list[str] = []
    targeted = [leg for leg in checked if leg.get("role") == "targeted" and leg.get("status") == "pass"]
    targeted_lenses = set().union(*(_lenses(leg) for leg in targeted)) if targeted else set()
    minimum = TERMINAL_TARGETED_LENS_MINIMUM if risk_tier == "terminal" else TARGETED_LENS_MINIMUM
    if len(targeted_lenses) < minimum:
        errors.append(f"{risk_tier} review requires at least {minimum} targeted lenses")

    other_primary = [leg for leg in checked if leg.get("role") == "other-primary"]
    passing_primary = [leg for leg in other_primary if leg.get("status") == "pass"]
    if passing_primary:
        family = _provider_family(passing_primary[0])
        if family not in PRIMARY_FAMILIES:
            errors.append("other-primary review must use a primary family")
        if chair_family and family == chair_family:
            errors.append("other-primary review must use a distinct primary family")
    elif other_primary and all(leg.get("status") in OTHER_PRIMARY_FALLBACK_STATUSES for leg in other_primary):
        # Fallback: the recorded skip must carry a reason and be covered by same-family targeted review.
        if not all(_has_reason(leg) for leg in other_primary):
            errors.append("other-primary skip requires a recorded reason")
        elif not _covers_other_primary(checked, chair_family):
            errors.append("other-primary skip requires a passing same-family targeted review with a distinct lens")
    else:
        errors.append("substantial+ review requires passing other-primary coverage")

    distinct = [leg for leg in checked if leg.get("role") == "distinct-family" and leg.get("status") == "pass"]
    for leg in distinct:
        if _provider_family(leg) in PRIMARY_FAMILIES | {chair_family}:
            errors.append("distinct-family review must use a non-primary family")
            break

    if risk_tier == "terminal":
        pressure_lenses = {lens.lower() for lens in targeted_lenses}
        if not any(
            any(marker in lens for marker in TERMINAL_PRESSURE_MARKERS)
            for lens in pressure_lenses
        ):
            errors.append("terminal review requires adversarial targeted pressure")

    if risk_tier in {"crucial", "terminal"}:
        recorded_skip = any(
            leg.get("role") == "distinct-family"
            and leg.get("status") in SKIPPED_STATUSES
            and isinstance(leg.get("reason"), str)
            and bool(leg.get("reason"))
            for leg in checked
        )
        if not distinct and not recorded_skip:
            errors.append(f"{risk_tier} review requires a distinct-family review or recorded skip")

    return errors
