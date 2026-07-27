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


def _lenses(leg: Mapping[str, Any]) -> set[str]:
    values = leg.get("lenses", ())
    if isinstance(values, str):
        values = (values,)
    if not isinstance(values, Iterable):
        return set()
    return {value for value in values if isinstance(value, str) and value}


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
    if not passing_primary:
        errors.append("substantial+ review requires passing other-primary coverage")
    else:
        family = passing_primary[0].get("family")
        if family not in PRIMARY_FAMILIES:
            errors.append("other-primary review must use a primary family")
        if chair_family and family == chair_family:
            errors.append("other-primary review must use a distinct primary family")

    distinct = [leg for leg in checked if leg.get("role") == "distinct-family" and leg.get("status") == "pass"]
    for leg in distinct:
        if leg.get("family") in PRIMARY_FAMILIES | {chair_family}:
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
