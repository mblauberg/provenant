#!/usr/bin/env python3
"""Declarative review-panel presets, reducers, and receipt validation.

This module does not dispatch reviewers. The chair selects a preset, records
the review records that form its membership, and fills the reducer output.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping

from .review_ladder import REVIEW_PLAN_STATUSES, SKIPPED_STATUSES


PANEL_AXES = (
    "membership",
    "distribution",
    "reduction",
    "degradation",
)

PANEL_PRESETS: dict[str, dict[str, object]] = {
    "council": {
        "distribution": "shared",
        "reduction": "agreements-conflicts",
        "minimum_members": 3,
    },
    "breadth": {
        "distribution": "split",
        "reduction": "union",
        "minimum_members": 1,
    },
}

PANEL_SPEC_KEYS = frozenset({
    "preset",
    "membership",
    "distribution",
    "reduction",
    "degradation",
})
PANEL_RESULT_KEYS = frozenset({"shortfall", "output"})
PANEL_RECORD_KEYS = frozenset({"id", "spec", "result"})


def known_distributions() -> frozenset[str]:
    """Valid distributions are exactly those some preset declares.

    Derived rather than listed so a new preset row admits its own distribution
    without a second edit, while an override naming a distribution no preset
    declares -- a typo -- is still rejected. `distribution` is descriptive: this
    module never branches on it, so nothing else would catch a misspelling.
    """

    return frozenset(
        row["distribution"]
        for row in PANEL_PRESETS.values()
        if isinstance(row.get("distribution"), str) and row["distribution"]
    )


def _string_list(value: object, field: str) -> list[str]:
    if not isinstance(value, list) or any(
        not isinstance(item, str) or not item for item in value
    ):
        raise ValueError(f"{field} must be a list of non-empty strings")
    return value


def _agreements_conflicts(output: object) -> dict[str, list[str]]:
    """Normalize a chair-authored evidence map without voting or scoring."""

    keys = {"agreements", "conflicts", "minority_views"}
    if not isinstance(output, dict) or set(output) != keys:
        raise ValueError(
            "agreements-conflicts output must contain agreements, conflicts, and minority_views"
        )
    return {
        field: list(_string_list(output[field], field))
        for field in ("agreements", "conflicts", "minority_views")
    }


def _union(output: object) -> dict[str, list[str]]:
    """Normalize the breadth preset's deduplicated findings union."""

    if not isinstance(output, dict) or set(output) != {"findings"}:
        raise ValueError("union output must contain findings")
    findings = _string_list(output["findings"], "findings")
    return {"findings": list(dict.fromkeys(findings))}


REDUCERS: dict[str, Callable[[object], dict[str, list[str]]]] = {
    "agreements-conflicts": _agreements_conflicts,
    "union": _union,
}


def resolve_panel(
    preset: str,
    overrides: Mapping[str, object] | None = None,
    **axis_overrides: object,
) -> dict[str, object]:
    """Resolve one preset, then apply only explicit per-axis overrides."""

    if preset not in PANEL_PRESETS:
        raise ValueError(f"unknown panel preset: {preset}")
    if overrides is not None and axis_overrides:
        raise ValueError("use one panel override form")
    if overrides is not None and (
        not isinstance(overrides, Mapping) or set(overrides) - set(PANEL_AXES)
    ):
        raise ValueError("panel overrides must name only panel axes")
    if set(axis_overrides) - set(PANEL_AXES):
        raise ValueError("panel overrides must name only panel axes")

    row = PANEL_PRESETS[preset]
    spec: dict[str, object] = {
        "preset": preset,
        "membership": [],
        "distribution": row["distribution"],
        "reduction": row["reduction"],
        "degradation": {"minimum_members": row["minimum_members"]},
    }
    if overrides:
        spec.update(overrides)
    spec.update(axis_overrides)
    if isinstance(spec["membership"], list):
        spec["membership"] = list(spec["membership"])
    if isinstance(spec["degradation"], Mapping):
        spec["degradation"] = dict(spec["degradation"])
    return spec


def validate_panel(spec: object, members: object) -> list[str]:
    """Return machine-checkable errors for a resolved panel and its members.

    User-authored data is reported as errors. A below-minimum completed-member
    count is deliberately not an error; it is represented by the panel result.
    """

    errors: list[str] = []
    if not isinstance(spec, dict) or set(spec) != PANEL_SPEC_KEYS:
        return ["panel spec must use the closed panel spec schema"]

    preset = spec.get("preset")
    if not isinstance(preset, str) or preset not in PANEL_PRESETS:
        errors.append("panel spec.preset is unknown")

    membership = spec.get("membership")
    valid_membership = isinstance(membership, list) and all(
        isinstance(member_id, str) and bool(member_id)
        for member_id in membership
    )
    if not valid_membership:
        errors.append("panel spec.membership must be a list of non-empty review ids")
        declared: list[str] = []
    else:
        declared = membership
        if len(set(declared)) != len(declared):
            errors.append("panel spec.membership review ids must be unique")

    distribution = spec.get("distribution")
    if not isinstance(distribution, str) or distribution not in known_distributions():
        errors.append("panel spec.distribution is unknown")
    reduction = spec.get("reduction")
    if not isinstance(reduction, str) or reduction not in REDUCERS:
        errors.append("panel spec.reduction is unknown")

    degradation = spec.get("degradation")
    if not isinstance(degradation, dict) or set(degradation) != {"minimum_members"}:
        errors.append("panel spec.degradation must contain minimum_members")
    else:
        minimum = degradation["minimum_members"]
        if (
            not isinstance(minimum, int)
            or isinstance(minimum, bool)
            or minimum < 1
        ):
            errors.append("panel spec.degradation.minimum_members must be a positive integer")

    if not isinstance(members, list):
        return errors + ["panel members must be a list"]

    seen: set[str] = set()
    actual: list[str] = []
    for index, member in enumerate(members):
        if not isinstance(member, Mapping):
            errors.append(f"panel members[{index}] must reference a review record")
            continue
        review_id = member.get("id")
        if not isinstance(review_id, str) or not review_id:
            errors.append(f"panel members[{index}].id must be a non-empty review id")
        elif review_id in seen:
            errors.append(f"panel members[{index}].id must be unique")
        else:
            seen.add(review_id)
            actual.append(review_id)
        status = member.get("status")
        if not isinstance(status, str) or status not in REVIEW_PLAN_STATUSES:
            errors.append(f"panel members[{index}].status is invalid")

    if valid_membership and set(actual) != set(declared):
        errors.append("panel members do not match spec.membership")
    return errors


def panel_shortfall(
    spec: Mapping[str, object],
    members: list[Mapping[str, object]],
) -> dict[str, int] | None:
    """Return the explicit non-blocking degradation carried by a result."""

    degradation = spec["degradation"]
    assert isinstance(degradation, Mapping)
    minimum = degradation["minimum_members"]
    assert isinstance(minimum, int)
    completed = sum(member.get("status") not in SKIPPED_STATUSES for member in members)
    if completed >= minimum:
        return None
    return {
        "minimum_members": minimum,
        "completed_members": completed,
        "missing_members": minimum - completed,
    }


def panel_result(
    spec: object,
    members: object,
    output: object,
) -> dict[str, object]:
    """Build the reducer-neutral result wrapper a chair records."""

    errors = validate_panel(spec, members)
    if errors:
        raise ValueError("; ".join(errors))
    assert isinstance(spec, dict)
    assert isinstance(members, list)
    reduction = spec["reduction"]
    assert isinstance(reduction, str)
    reducer = REDUCERS[reduction]
    return {
        "shortfall": panel_shortfall(spec, members),
        "output": reducer(output),
    }


def validate_panel_result(
    spec: object,
    members: object,
    result: object,
) -> list[str]:
    """Validate a recorded result against its declared panel and reviews."""

    errors = validate_panel(spec, members)
    if errors:
        return errors
    if not isinstance(result, dict) or set(result) != PANEL_RESULT_KEYS:
        return ["panel result must use the closed panel result schema"]

    assert isinstance(spec, dict)
    assert isinstance(members, list)
    expected_shortfall = panel_shortfall(spec, members)
    if result.get("shortfall") != expected_shortfall:
        errors.append("panel result.shortfall does not match completed membership")

    reduction = spec["reduction"]
    assert isinstance(reduction, str)
    try:
        normalized = REDUCERS[reduction](result.get("output"))
    except (KeyError, TypeError, ValueError) as exc:
        errors.append(f"panel result.output is invalid: {exc}")
    else:
        if normalized != result.get("output"):
            errors.append("panel result.output is not normalized by its declared reducer")
    return errors
