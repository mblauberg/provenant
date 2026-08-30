"""Review-ladder validation for delivery receipts."""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any

from delivery_validation_common import (
    REVIEW_ROLES,
    Invalid,
    _list,
    _mapping,
    _safe_path,
    check_review_ladder,
    fail,
)

def _validate_route(
    review: dict[str, Any], linked: dict[str, Any], artifacts: dict[str, dict[str, Any]],
    artifact_root: Path | None, verify_hashes: bool,
) -> None:
    route_ref = _mapping(linked.get("route_receipt"), f"review {review.get('id')} route_receipt")
    route_path = _safe_path(route_ref.get("path"), f"review {review.get('id')} route path")
    route_digest = route_ref.get("digest")
    fail(route_path not in linked.get("source_paths", []), f"review {review.get('id')} route receipt must be listed as a source")
    route_artifact = next(
        (item for item in artifacts.values() if item.get("path") == route_path), None,
    )
    fail(not route_artifact, f"review {review.get('id')} route receipt artifact is missing")
    fail(route_artifact.get("digest") != route_digest, f"review {review.get('id')} route receipt digest is not bound")
    fail(review.get("route_receipt_digest") != route_digest, f"review {review.get('id')} route receipt digest is not bound")
    if artifact_root is None:
        return
    target = (artifact_root / route_path).resolve()
    try:
        target.relative_to(artifact_root.resolve())
    except ValueError as exc:
        raise Invalid(f"review {review.get('id')} route receipt escapes workspace") from exc
    if not target.is_file():
        if verify_hashes:
            raise Invalid(f"review {review.get('id')} route receipt is missing")
        return
    raw = target.read_bytes()
    if verify_hashes:
        actual = "sha256:" + hashlib.sha256(raw).hexdigest()
        fail(actual != route_digest, f"review {review.get('id')} route receipt digest does not match live bytes")
    try:
        route = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise Invalid(f"review {review.get('id')} route receipt is not readable JSON") from exc
    fail(
        not isinstance(route, dict)
        or route.get("status") != "ok"
        or route.get("adapter") != review.get("adapter")
        or route.get("reviewer_id") != review.get("reviewer_id")
        or route.get("resolved_model", route.get("model")) != review.get("model")
        or route.get("model_family") != review.get("provider_family")
        or route.get("certification_eligible") is not True,
        f"review {review.get('id')} route receipt identity does not match review lineage",
    )
    if review.get("role") == "other-primary":
        fail(route.get("cross_family") is not True, "other-primary review requires a cross-family route receipt")


def _validate_reviews(
    run: dict[str, Any], evidence: dict[str, dict[str, Any]], *, required: bool,
    artifacts: dict[str, dict[str, Any]] | None = None,
    artifact_root: Path | None = None, verify_hashes: bool = False,
) -> None:
    artifacts = artifacts or {
        item["id"]: item for item in run.get("artifacts", [])
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    }
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
            _validate_route(item, linked, artifacts, artifact_root, verify_hashes)
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
