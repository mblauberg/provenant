"""Review-ladder validation for delivery receipts."""

from __future__ import annotations

from typing import Any

from delivery_validation_common import (
    REVIEW_ROLES,
    Invalid,
    _list,
    _mapping,
    check_review_ladder,
    fail,
)

def _validate_reviews(run: dict[str, Any], evidence: dict[str, dict[str, Any]], *, required: bool) -> None:
    reviews = []
    for index, raw in enumerate(_list(run.get("reviews"), "reviews")):
        item = _mapping(raw, f"reviews[{index}]")
        fail(item.get("status") not in {"pass", "failed", "unavailable", "skipped"}, f"review {index} status is invalid")
        fail(not item.get("provider_family") or not item.get("adapter") or not item.get("role"), f"review {index} lacks lineage")
        fail(item.get("role") not in REVIEW_ROLES, f"review {index} role is invalid")
        fail(item.get("independent_of_authorship") is not True, f"review {index} is not independent")
        fail(not item.get("lenses"), f"review {index} requires lenses")
        if item.get("status") == "pass":
            fail(not item.get("model"), f"passing review {index} requires actual model identity")
            fail(item.get("evidence_id") not in evidence, f"review {index} must link evidence")
            linked = evidence[item["evidence_id"]]
            fail(linked.get("status") != "pass" or linked.get("kind") != "judgement", f"passing review {index} must link passing judgement evidence")
            lineage = _mapping(linked.get("model_lineage"), f"review {index} evidence lineage")
            fail(
                lineage.get("adapter") != item.get("adapter")
                or lineage.get("provider_family") != item.get("provider_family")
                or lineage.get("model") != item.get("model"),
                f"review {index} lineage does not match its evidence",
            )
        else:
            fail(not item.get("reason"), f"non-passing review {index} requires reason")
        reviews.append(item)
    optional = [item for item in reviews if item.get("role") == "distinct-family"]
    fail(any(item.get("provider_family") in {"openai", "anthropic"} for item in optional), "distinct-family review must use a non-primary family")
    if not required:
        return
    chair_family = run.get("chair_family")
    legs = []
    for item in reviews:
        role = item.get("role")
        ladder_role = "other-primary" if role == "other-primary" else "distinct-family" if role == "distinct-family" else "targeted"
        legs.append({
            "role": ladder_role,
            "family": item.get("provider_family"),
            # The delivery-run schema calls an intentional omission "skipped";
            # normalize it to the shared review-plan ladder vocabulary.
            "status": "omitted" if item.get("status") == "skipped" else item.get("status"),
            "lenses": item.get("lenses", []),
            "reason": item.get("reason"),
        })
    ladder_errors = check_review_ladder(run.get("risk_tier"), legs, chair_family=chair_family)
    if ladder_errors:
        raise Invalid(ladder_errors[0])


