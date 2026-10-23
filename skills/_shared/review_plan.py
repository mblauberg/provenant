#!/usr/bin/env python3
"""Review-plan topology validation for a bounded run receipt.

Callers pass the receipt's ``review_plan`` mapping. Referenced evidence,
terminal results and route receipts are re-read through the run's custody
helpers, so the caller owns the run directory and the receipt shape.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path

from .custody import OwnedFileError, read_contained_regular
from .review_ladder import REVIEW_PLAN_STATUSES, SKIPPED_STATUSES, check_review_ladder
from .review_panel import PANEL_RECORD_KEYS, validate_panel_result
from .review_terminal import REVIEW_RESULT_KEYS, normalise_dispatch_review

# A review tier may use the tier name or the adapter alias it maps to.
REVIEW_TIERS = {
    "junior": "junior", "senior": "senior", "principal": "principal",
    "junior": "junior", "senior": "senior", "principal": "principal",
}


def _inside(root: Path, candidate: Path) -> bool:
    try:
        candidate.resolve().relative_to(root.resolve())
        return True
    except ValueError:
        return False


def _table(text: str, required: list[str]) -> list[dict[str, str]]:
    lines = [line for line in text.splitlines() if line.strip().startswith("|")]
    for index, line in enumerate(lines):
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if cells == required:
            rows: list[dict[str, str]] = []
            for row in lines[index + 2 :]:
                values = [cell.strip() for cell in row.strip().strip("|").split("|")]
                if len(values) != len(required):
                    continue
                rows.append(dict(zip(required, values)))
            return rows
    raise ValueError("required table header not found: " + ", ".join(required))


def _owned_bytes(run_dir: Path, value: object, label: str) -> tuple[str, bytes]:
    if not isinstance(value, (str, Path)):
        raise OwnedFileError(f"{label} path is missing")
    relative, _target, data = read_contained_regular(run_dir, value, label=label)
    return relative, data


def validate_review_plan(raw: object, run_dir: Path | None = None) -> list[str]:
    errors: list[str] = []
    if not isinstance(raw, dict) or set(raw) != {
        "risk_tier", "chair_family", "concurrency_ceiling", "reviews", "panels",
    }:
        return ["receipt review_plan must use the closed review topology schema"]
    risk = raw.get("risk_tier")
    if not isinstance(risk, str) or risk not in {"routine", "substantial", "crucial", "terminal"}:
        errors.append("receipt review_plan.risk_tier is invalid")
    ceiling = raw.get("concurrency_ceiling")
    if not isinstance(ceiling, int) or isinstance(ceiling, bool) or not 1 <= ceiling <= 32:
        errors.append("receipt review_plan.concurrency_ceiling must be between 1 and 32")
    reviews = raw.get("reviews")
    if not isinstance(reviews, list):
        return errors + ["receipt review_plan.reviews must be a list"]
    keys = {
        "id", "scope", "lens", "family", "tier", "status",
        "substitution_for", "evidence", "reason", "verdict", "terminal_result", "wave",
        "adapter", "model", "catalog_model", "route_receipt",
        "reviewer_id", "adapter_gate",
    }
    seen: set[str] = set()
    checked: list[dict[str, object]] = []
    normalized_legs: dict[str, dict[str, object]] = {}
    for index, review in enumerate(reviews):
        if not isinstance(review, dict) or set(review) != keys:
            errors.append(f"receipt review_plan.reviews[{index}] must use the closed review record schema")
            continue
        if not isinstance(review["id"], str) or not review["id"] or review["id"] in seen:
            errors.append(f"receipt review_plan.reviews[{index}].id must be non-empty and unique")
        else:
            seen.add(review["id"])
        if not isinstance(review["scope"], str) or review["scope"] not in {"targeted", "primary"}:
            errors.append(f"receipt review_plan.reviews[{index}].scope is invalid")
        if not isinstance(review["tier"], str) or review["tier"] not in REVIEW_TIERS:
            errors.append(f"receipt review_plan.reviews[{index}].tier is invalid")
        if not isinstance(review["status"], str) or review["status"] not in REVIEW_PLAN_STATUSES:
            errors.append(f"receipt review_plan.reviews[{index}].status is invalid")
        for field in ("lens", "family"):
            if not isinstance(review[field], str) or not review[field].strip():
                errors.append(f"receipt review_plan.reviews[{index}].{field} is required")
        if not isinstance(review["adapter"], str) or not review["adapter"]:
            errors.append(f"receipt review_plan.reviews[{index}].adapter is required")
        if (
            not isinstance(review["adapter_gate"], str)
            or review["adapter_gate"] not in {"fabric", "direct-cli"}
        ):
            errors.append(f"receipt review_plan.reviews[{index}].adapter_gate is invalid")
        if not isinstance(review["reviewer_id"], str) or not review["reviewer_id"]:
            errors.append(f"receipt review_plan.reviews[{index}].reviewer_id is required")
        if review["status"] == "complete" and not any(
            isinstance(review[field], str) and review[field] for field in ("model", "catalog_model")
        ):
            errors.append(f"receipt review_plan.reviews[{index}] requires resolved or catalog model identity")
        if review["status"] == "complete" and (
            not isinstance(review["verdict"], str) or not review["verdict"].strip()
        ):
            errors.append(f"receipt review_plan.reviews[{index}] requires an explicit verdict")
        if review["status"] == "complete" and not isinstance(review["terminal_result"], dict):
            errors.append(f"receipt review_plan.reviews[{index}] requires retained terminal_result")
        terminal_result_ref = review["terminal_result"]
        terminal_result_value: object = None
        terminal_target: Path | None = None
        if review["status"] == "complete":
            if not isinstance(terminal_result_ref, dict) or set(terminal_result_ref) != {"path", "digest"}:
                errors.append(f"receipt review_plan.reviews[{index}].terminal_result is invalid")
            else:
                terminal_path = Path(terminal_result_ref["path"]) if isinstance(terminal_result_ref["path"], str) else Path("..")
                terminal_digest = terminal_result_ref["digest"]
                if (
                    terminal_path.is_absolute()
                    or ".." in terminal_path.parts
                    or not isinstance(terminal_digest, str)
                    or not terminal_digest.startswith("sha256:")
                ):
                    errors.append(f"receipt review_plan.reviews[{index}].terminal_result is invalid")
                elif run_dir is not None:
                    try:
                        terminal_rel, terminal_bytes = _owned_bytes(
                            run_dir, terminal_path, f"review {index} terminal result"
                        )
                        terminal_target = run_dir / terminal_rel
                    except OwnedFileError:
                        terminal_target = None
                        terminal_bytes = None
                    if terminal_target is None or "sha256:" + hashlib.sha256(terminal_bytes).hexdigest() != terminal_digest:
                        errors.append(f"receipt review_plan.reviews[{index}].terminal_result is missing or does not match")
                    else:
                        try:
                            terminal_result_value = json.loads(terminal_bytes.decode("utf-8"))
                        except (UnicodeDecodeError, json.JSONDecodeError):
                            errors.append(f"receipt review_plan.reviews[{index}].terminal_result is invalid JSON")
                        if not isinstance(terminal_result_value, dict) or set(terminal_result_value) != REVIEW_RESULT_KEYS:
                            errors.append(f"receipt review_plan.reviews[{index}].terminal_result is invalid")
        evidence = review["evidence"]
        evidence_bytes: bytes | None = None
        if not isinstance(evidence, dict) or set(evidence) != {"path", "digest"}:
            errors.append(f"receipt review_plan.reviews[{index}].evidence is invalid")
        else:
            path = Path(evidence["path"]) if isinstance(evidence["path"], str) else Path("..")
            digest = evidence["digest"]
            if path.is_absolute() or ".." in path.parts or not isinstance(digest, str) or not digest.startswith("sha256:"):
                errors.append(f"receipt review_plan.reviews[{index}].evidence is invalid")
            elif run_dir is not None and review["status"] == "complete":
                try:
                    _evidence_rel, evidence_bytes = _owned_bytes(run_dir, path, f"review {index} evidence")
                except OwnedFileError:
                    evidence_bytes = None
                if evidence_bytes is None or "sha256:" + hashlib.sha256(evidence_bytes).hexdigest() != digest:
                    errors.append(f"receipt review_plan.reviews[{index}].evidence is missing or does not match")
        if isinstance(review["status"], str) and review["status"] in SKIPPED_STATUSES and (
            not isinstance(review["reason"], str) or not review["reason"]
        ):
            errors.append(f"receipt review_plan.reviews[{index}].reason is required for an incomplete leg")
        route = review["route_receipt"]
        if not isinstance(route, dict) or set(route) != {"path", "digest"}:
            errors.append(f"receipt review_plan.reviews[{index}].route_receipt is invalid")
        else:
            route_path = Path(route["path"]) if isinstance(route["path"], str) else Path("..")
            route_digest = route["digest"]
            if route_path.is_absolute() or ".." in route_path.parts or not isinstance(route_digest, str) or not route_digest.startswith("sha256:"):
                errors.append(f"receipt review_plan.reviews[{index}].route_receipt is invalid")
            elif run_dir is not None and review["status"] == "complete":
                try:
                    _route_rel, route_bytes = _owned_bytes(run_dir, route_path, f"review {index} route receipt")
                except OwnedFileError:
                    route_bytes = None
                if route_bytes is None or "sha256:" + hashlib.sha256(route_bytes).hexdigest() != route_digest:
                    errors.append(f"receipt review_plan.reviews[{index}].route_receipt is missing or does not match")
                else:
                    try:
                        route_value = json.loads(route_bytes.decode("utf-8"))
                    except (UnicodeDecodeError, json.JSONDecodeError):
                        errors.append(f"receipt review_plan.reviews[{index}].route_receipt is invalid JSON")
                    else:
                        if not isinstance(route_value, dict):
                            errors.append(f"receipt review_plan.reviews[{index}].route_receipt identity does not match")
                        elif (
                            route_value.get("status") != "ok"
                            or route_value.get("adapter") != review["adapter"]
                            or route_value.get("adapter_gate") != review["adapter_gate"]
                            or route_value.get("reviewer_id") != review["reviewer_id"]
                            or route_value.get("resolved_model", route_value.get("model", "")) != review["model"]
                            or route_value.get("catalog_model", "") != review["catalog_model"]
                            or route_value.get("model_family") != review["family"]
                            or not isinstance(route_value.get("provider_family"), str)
                            or not route_value.get("provider_family", "").strip()
                            or not isinstance(route_value.get("endpoint_provider"), str)
                            or not route_value.get("endpoint_provider", "").strip()
                            or not isinstance(route_value.get("output_digest"), str)
                            or not route_value.get("output_digest", "").startswith("sha256:")
                            or (
                                isinstance(raw.get("chair_family"), str)
                                and bool(raw.get("chair_family"))
                                and route_value.get("orchestrator_family") != raw.get("chair_family")
                            )
                        ):
                            errors.append(f"receipt review_plan.reviews[{index}].route_receipt identity does not match")
                        if isinstance(route_value, dict):
                            route_alias = route_value.get("route_alias", route_value.get("alias"))
                            if REVIEW_TIERS.get(review["tier"]) == "principal" and route_alias != "principal":
                                errors.append(f"receipt review_plan.reviews[{index}].route_receipt does not prove principal strength")
                            transcript_available = bool(
                                evidence_bytes is not None and evidence_bytes
                            )
                            output_value = route_value.get("output_path")
                            output_target = (
                                Path(output_value)
                                if isinstance(output_value, str) and output_value
                                else None
                            )
                            if output_target is not None and not output_target.is_absolute():
                                output_target = run_dir / output_target
                            if (
                                terminal_target is not None
                                and (
                                    output_target is None
                                    or output_target.resolve() != terminal_target.resolve()
                                )
                            ):
                                terminal_result_value = None
                                errors.append(
                                    f"receipt review_plan.reviews[{index}].terminal_result does not bind route output"
                                )
                            output_bytes: bytes | None = None
                            if output_target is not None and _inside(run_dir, output_target):
                                try:
                                    _output_rel, output_bytes = _owned_bytes(
                                        run_dir, output_target.relative_to(run_dir),
                                        f"review {index} route output",
                                    )
                                except (OwnedFileError, ValueError):
                                    output_bytes = None
                            if output_bytes is not None:
                                output_digest = "sha256:" + hashlib.sha256(output_bytes).hexdigest()
                                if output_digest != route_value.get("output_digest"):
                                    terminal_result_value = None
                                    errors.append(
                                        f"receipt review_plan.reviews[{index}].route output digest does not match dispatch receipt"
                                    )
                                elif (
                                    isinstance(terminal_result_ref, dict)
                                    and terminal_result_ref.get("digest") != output_digest
                                ):
                                    terminal_result_value = None
                                    errors.append(
                                        f"receipt review_plan.reviews[{index}].terminal_result digest does not match dispatch output"
                                    )
                            dispatcher_output_available = bool(
                                output_bytes is not None and output_bytes
                            )
                            leg = normalise_dispatch_review(
                                route_value,
                                terminal_result_value,
                                review_verdict=review["verdict"],
                                chair_family=raw.get("chair_family"),
                                transcript_available=transcript_available,
                                dispatcher_output_available=dispatcher_output_available,
                            )
                            if (
                                leg["status"] == "pass"
                                and raw.get("chair_family")
                                and leg["provider_family"] != raw.get("chair_family")
                                and route_value.get("cross_family") is not True
                            ):
                                leg["status"] = "failed"
                                leg["certifying_vote"] = False
                                leg["reason"] = "cross-family review requires a cross-family route receipt"
                            if isinstance(review["id"], str):
                                normalized_legs[review["id"]] = leg
                            if leg["status"] != "pass":
                                errors.append(
                                    f"receipt review_plan.reviews[{index}] is not a certifying review leg: {leg['reason']}"
                                )
                            if (
                                raw.get("chair_family")
                                and leg["provider_family"] != raw.get("chair_family")
                                and not leg["certifying_vote"]
                            ):
                                errors.append(
                                    f"receipt review_plan.reviews[{index}] route is not certification eligible: "
                                    f"{leg['reason'] or 'provider-lineage'}"
                                )
        if not isinstance(review["wave"], int) or isinstance(review["wave"], bool) or review["wave"] < 0:
            errors.append(f"receipt review_plan.reviews[{index}].wave must be a non-negative integer")
        if all(isinstance(review[field], str) for field in (
            "id", "scope", "lens", "family", "tier", "status", "substitution_for",
            "reason", "verdict", "adapter", "model", "catalog_model",
            "reviewer_id", "adapter_gate",
        )) and isinstance(review["wave"], int) and not isinstance(review["wave"], bool):
            checked.append(review)
        if not isinstance(review["substitution_for"], str):
            errors.append(f"receipt review_plan.reviews[{index}].substitution_for must be a string")

    panels = raw.get("panels")
    if not isinstance(panels, list):
        errors.append("receipt review_plan.panels must be a list")
    else:
        panel_ids: set[str] = set()
        reviews_by_id = {
            review["id"]: review
            for review in checked
            if isinstance(review.get("id"), str) and review["id"]
        }
        for index, panel in enumerate(panels):
            prefix = f"receipt review_plan.panels[{index}]"
            if not isinstance(panel, dict) or set(panel) != PANEL_RECORD_KEYS:
                errors.append(f"{prefix} must use the closed panel record schema")
                continue
            panel_id = panel["id"]
            if (
                not isinstance(panel_id, str)
                or not panel_id
                or panel_id in panel_ids
            ):
                errors.append(f"{prefix}.id must be non-empty and unique")
            else:
                panel_ids.add(panel_id)

            spec = panel["spec"]
            membership = spec.get("membership") if isinstance(spec, dict) else None
            members: list[dict[str, object]] = []
            if isinstance(membership, list):
                for member_index, review_id in enumerate(membership):
                    if isinstance(review_id, str) and review_id in reviews_by_id:
                        members.append(reviews_by_id[review_id])
                    elif isinstance(review_id, str):
                        errors.append(
                            f"{prefix}.spec.membership[{member_index}] references unknown review id"
                        )
            panel_errors = validate_panel_result(spec, members, panel["result"])
            errors.extend(f"{prefix}: {error}" for error in panel_errors)
    completed = [r for r in checked if r["status"] == "complete"]
    if len({r["reviewer_id"] for r in completed}) != len(completed):
        errors.append("completed reviews require distinct reviewer_id values")
    for field in ("evidence", "route_receipt"):
        identities = [
            (r[field].get("path"), r[field].get("digest"))
            for r in completed
            if isinstance(r[field], dict)
            and isinstance(r[field].get("path"), str)
            and isinstance(r[field].get("digest"), str)
        ]
        digests = [identity[1] for identity in identities]
        if len(identities) != len(completed) or len(set(digests)) != len(completed):
            errors.append(f"completed reviews require distinct {field} artifacts")
    if not isinstance(risk, str) or risk not in {"substantial", "crucial", "terminal"}:
        return errors
    chair_value = raw.get("chair_family")
    if not isinstance(chair_value, str) or chair_value not in {"openai", "anthropic"}:
        errors.append("substantial review_plan requires an openai or anthropic chair_family")
    chair = chair_value if isinstance(chair_value, str) else ""
    other_primary = "anthropic" if chair == "openai" else "openai"
    legs: list[dict[str, object]] = []
    for review in checked:
        # Roles retain their receipt vocabulary; the shared HARNESS ladder
        # counts every passing independent leg by family and lens.
        if review["scope"] == "targeted":
            role = "targeted"
        elif review["family"] == other_primary:
            role = "other-primary"
        else:
            role = "distinct-family"
        normalized_leg = normalized_legs.get(review["id"]) if isinstance(review.get("id"), str) else None
        leg_status = (
            normalized_leg["status"]
            if normalized_leg is not None
            else "pass" if review["status"] == "complete" else review["status"]
        )
        if (
            normalized_leg is not None
            and normalized_leg["provider_family"] != chair
            and not normalized_leg["certifying_vote"]
        ):
            leg_status = "failed"
        legs.append({
            "role": role,
            "family": review["family"],
            "provider_family": (
                normalized_legs.get(review["id"], {}).get("provider_family", review["family"])
                if isinstance(review.get("id"), str)
                else review["family"]
            ),
            "endpoint_provider": (
                normalized_legs.get(review["id"], {}).get("endpoint_provider", "")
                if isinstance(review.get("id"), str)
                else ""
            ),
            "status": leg_status,
            "lenses": [review["lens"]],
            "reason": (
                normalized_leg["reason"]
                if normalized_leg is not None
                else review["reason"]
            ),
            "substitution_for": review["substitution_for"],
        })
    errors.extend(check_review_ladder(risk, legs, chair_family=chair))
    waves: dict[int, int] = {}
    for review in checked:
        if review["status"] != "omitted" and isinstance(review["wave"], int):
            waves[review["wave"]] = waves.get(review["wave"], 0) + 1
    if isinstance(ceiling, int) and any(count > ceiling for count in waves.values()):
        errors.append("review_plan observed wave exceeds concurrency_ceiling")
    return errors
