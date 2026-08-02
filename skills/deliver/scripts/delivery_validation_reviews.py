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

CERTIFYING_PROVIDER_ASSURANCE = frozenset({"full-vendor-identity"})


def _validate_route_receipt(
    run: dict[str, Any], item: dict[str, Any], linked: dict[str, Any], *,
    workspace_root: Path | None, artifacts: dict[str, dict[str, Any]] | None,
    verify_hashes: bool,
) -> None:
    fail(item.get("provider_family") == run.get("chair_family"), "other-primary review must use a distinct primary family")
    route_ref = _mapping(linked.get("route_receipt"), "review route receipt")
    fail(not isinstance(route_ref.get("path"), str) or not route_ref["path"], "other-primary review route receipt path is invalid")
    fail(not isinstance(route_ref.get("digest"), str), "other-primary review route receipt digest is invalid")
    route_path = _safe_path(route_ref["path"], "other-primary route receipt.path")
    fail(route_path not in linked.get("source_paths", []), "other-primary route receipt is not bound to review evidence")
    if artifacts is not None:
        fail(
            not any(
                artifact.get("path") == route_path
                and artifact.get("digest") == route_ref["digest"]
                for artifact in artifacts.values()
            ),
            "other-primary route receipt is not bound to a declared artifact",
        )
    if workspace_root is None or not verify_hashes:
        return
    target = (workspace_root / route_path).resolve()
    try:
        target.relative_to(workspace_root.resolve())
    except ValueError as exc:
        raise Invalid("other-primary route receipt resolves outside workspace_root") from exc
    try:
        raw = target.read_bytes()
        route = json.loads(raw)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise Invalid("other-primary route receipt is unreadable") from exc
    fail("sha256:" + hashlib.sha256(raw).hexdigest() != route_ref["digest"], "other-primary route receipt digest does not match live bytes")
    route = _mapping(route, "other-primary route receipt")
    fail(route.get("status") != "ok", "other-primary route receipt is not closed successfully")
    fail(
        route.get("cross_family") is not True
        or route.get("provider_assurance") not in CERTIFYING_PROVIDER_ASSURANCE,
        "other-primary review requires a closed cross-family route receipt",
    )
    fail(route.get("adapter") != item.get("adapter") or route.get("reviewer_id") != item.get("reviewer_id") or route.get("model_family") != item.get("provider_family") or route.get("resolved_model", route.get("model")) != item.get("model"), "other-primary route receipt identity does not match review lineage")


def _validate_reviews(
    run: dict[str, Any], evidence: dict[str, dict[str, Any]], *, required: bool,
    workspace_root: Path | None = None,
    artifacts: dict[str, dict[str, Any]] | None = None,
    verify_hashes: bool = False,
) -> None:
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
            if item.get("role") == "other-primary":
                _validate_route_receipt(
                    run, item, linked, workspace_root=workspace_root,
                    artifacts=artifacts, verify_hashes=verify_hashes,
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
