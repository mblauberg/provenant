"""Lifecycle operations for the delivery receipt producer.

This module reads the checked lifecycle and profile JSON documents directly. It
does not import the delivery validator, so the producer and validator remain
independent consumers of the written contracts.
"""

from __future__ import annotations

import json
import math
import copy
from pathlib import Path
import sys
from typing import Any


CERTIFYING_PROVIDER_ASSURANCE = frozenset({
    "full-vendor-identity",
    "lockfile-install-attestation",
})


SKILLS_ROOT = Path(__file__).resolve().parents[2]
if str(SKILLS_ROOT) not in sys.path:
    sys.path.insert(0, str(SKILLS_ROOT))


def _error(api: dict[str, Any], message: str) -> None:
    raise api["ReceiptError"](message)


def profile_requirements(
    run: dict[str, Any], profile_path: Path, error_type: type[ValueError],
) -> dict[str, Any]:
    try:
        registry = json.loads(profile_path.read_text())
        profile = registry["profiles"][run["profile"]]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        raise error_type(f"delivery profile registry is unreadable: {exc}") from exc
    if not isinstance(profile, dict):
        raise error_type("delivery profile is invalid")
    return profile


def required_profile_evidence(
    run: dict[str, Any], profile: dict[str, Any], artifacts: dict[str, dict[str, Any]],
) -> dict[str, set[str]]:
    required = {
        kind: set(gates)
        for kind, gates in profile.get("required_evidence", {}).items()
    }
    for artifact in artifacts.values():
        conditional = profile.get("conditional_evidence", {}).get(
            artifact.get("artifact_type"), {}
        )
        for kind, gates in conditional.items():
            required.setdefault(kind, set()).update(gates)
    required.setdefault("deterministic", set())
    required.setdefault("judgement", set())
    return required


def review_ladder_error(run: dict[str, Any]) -> str | None:
    legs = []
    for item in run.get("reviews", []):
        if not isinstance(item, dict):
            continue
        legs.append({
            "role": item.get("role"),
            "family": item.get("provider_family"),
            "status": "omitted" if item.get("status") == "skipped" else item.get("status"),
            "lenses": item.get("lenses", []),
            "reason": item.get("reason"),
        })
    try:
        from _shared.review_ladder import check_review_ladder
        errors = check_review_ladder(
            run.get("risk_tier"), legs, chair_family=run.get("chair_family"),
        )
    except (ImportError, TypeError, KeyError):
        errors = []
    return errors[0] if errors else None


def _evidence_map(run: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {
        item["id"]: item
        for item in run.get("evidence", [])
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    }


def _artifact_map(run: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {
        item["id"]: item
        for item in run.get("artifacts", [])
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    }


def _next_bundle_version(run: dict[str, Any], artifact_id: str, api: dict[str, Any]) -> str:
    existing = {
        item.get("id") for item in run.get("artifacts", [])
        if isinstance(item, dict)
    }
    version = 2
    while True:
        candidate = api["require_identifier"](f"{artifact_id}-v{version}", "bundle artifact id")
        if candidate not in existing:
            return candidate
        version += 1


def _passing(run: dict[str, Any], *, kind: str | None = None, gate: str | None = None):
    return [
        item for item in run.get("evidence", [])
        if isinstance(item, dict)
        and item.get("status") == "pass"
        and (kind is None or item.get("kind") == kind)
        and (gate is None or item.get("gate") == gate)
    ]


def _live_bound_artifact_error(
    run: dict[str, Any], workspace: Path, api: dict[str, Any],
) -> str | None:
    """Re-hash every path-backed binding while the receipt lock is held."""
    for index, raw in enumerate(run.get("artifacts", [])):
        if not isinstance(raw, dict) or not raw.get("path"):
            continue
        try:
            target, _ = api["safe_workspace_path"](workspace, raw["path"], f"artifact {raw.get('id', index)}")
            api["ensure_allowed_artifact_target"](run, workspace, target)
            actual = api["digest_bytes"](target.read_bytes())
        except (OSError, api["ReceiptError"]) as exc:
            return f"bound artifact {raw.get('id', index)} is unreadable: {exc}"
        if actual != raw.get("digest"):
            return f"bound artifact {raw.get('id', index)} digest does not match live bytes"
    return None


def measures_ready(
    run: dict[str, Any], profile: dict[str, Any], cited: set[str],
) -> bool:
    evidence = _evidence_map(run)
    measures = run.get("measures")
    if not isinstance(measures, dict):
        return False
    for kind in ("outcome", "trajectory"):
        rows = measures.get(kind)
        required = set(profile.get("required_measures", {}).get(kind, []))
        if not isinstance(rows, list):
            return False
        ids = {item.get("id") for item in rows if isinstance(item, dict)}
        if not required <= ids:
            return False
        for item in rows:
            if not isinstance(item, dict):
                return False
            linked = evidence.get(item.get("evidence_id"))
            if (
                item.get("status") != "pass"
                or not linked
                or linked.get("status") != "pass"
                or item.get("evidence_kind") != linked.get("kind")
                or item.get("evidence_id") not in cited
                or "value" not in item
                or not item.get("target")
                or not item.get("aggregation")
            ):
                return False
    return True


# Compatibility name used by the lifecycle contract's earlier producer
# integration tests. The implementation remains local to the producer.
def measures_gate_ready(
    run: dict[str, Any], profile: dict[str, Any], cited: set[str],
) -> bool:
    return measures_ready(run, profile, cited)


def observation_ready(run: dict[str, Any], cited: set[str]) -> bool:
    observation = run.get("observation")
    if not isinstance(observation, dict) or observation.get("status") != "pass":
        return False
    ids = observation.get("evidence_ids")
    signals = observation.get("signals")
    thresholds = observation.get("thresholds")
    evidence = _evidence_map(run)
    if (
        not isinstance(ids, list) or not ids or not set(ids) <= cited
        or not isinstance(signals, list) or not signals
        or not isinstance(thresholds, dict) or set(signals) != set(thresholds)
    ):
        return False
    for evidence_id in ids:
        item = evidence.get(evidence_id)
        if not item or item.get("kind") != "observation" or item.get("status") != "pass":
            return False
        threshold = thresholds.get(item.get("gate"), {})
        value = item.get("measured_value")
        limit = threshold.get("limit") if isinstance(threshold, dict) else None
        if (
            not isinstance(value, (int, float)) or isinstance(value, bool)
            or not math.isfinite(value) or not isinstance(limit, (int, float))
            or isinstance(limit, bool) or not math.isfinite(limit)
        ):
            return False
        direction = threshold.get("direction")
        if direction == "gte" and value < limit:
            return False
        if direction == "lte" and value > limit:
            return False
        if direction == "eq" and value != limit:
            return False
        if direction not in {"gte", "lte", "eq"}:
            return False
    return True


def transition_gate_error(
    run: dict[str, Any], target: str, cited: set[str], api: dict[str, Any],
    *, workspace: Path | None = None,
) -> str | None:
    evidence = _evidence_map(run)
    live_error = _live_bound_artifact_error(run, workspace or api.get("workspace", Path.cwd()), api)
    if live_error:
        return live_error
    profile = profile_requirements(run, api["PROFILE_PATH"], api["ReceiptError"])
    required = required_profile_evidence(run, profile, _artifact_map(run))
    if target == "approved":
        if not (
            run.get("intent", {}).get("approval", {}).get("status") == "approved"
            and any(item.get("id") in cited for item in _passing(run, kind="human", gate="intent-approval"))
        ):
            return "approved gate is not satisfied"
        if run.get("risk_tier") in {"substantial", "crucial", "terminal"}:
            design = run.get("design", {})
            design_evidence = evidence.get(design.get("evidence"))
            artifact = _artifact_map(run).get(design.get("artifact_id"))
            if not (
                design.get("status") == "approved"
                and design_evidence
                and design_evidence.get("kind") == "human"
                and design_evidence.get("status") == "pass"
                and design_evidence.get("gate") == "design-approval"
                and design.get("evidence") in cited
                and artifact
                and design.get("digest") == artifact.get("digest")
            ):
                return "approved design gate is not satisfied"
    elif target == "reviewing":
        rows = [
            item for item in _passing(run, kind="deterministic")
            if item.get("gate") in required["deterministic"]
        ]
        if {item.get("gate") for item in rows} != required["deterministic"] or not {
            item.get("id") for item in rows
        } <= cited:
            return "reviewing deterministic gate is not satisfied"
        latest_repair = next(
            (item for item in reversed(run.get("state_history", []))
             if isinstance(item, dict) and item.get("state") == "repairing"),
            None,
        )
        if latest_repair:
            repair_at = api["parse_utc"](latest_repair.get("at"), "repairing timestamp")
            for item in rows:
                finished = item.get("finished_at")
                if not finished or api["parse_utc"](finished, "evidence finished_at") < repair_at:
                    return (
                        f"deterministic gate {item.get('gate')} evidence predates "
                        f"repairing transition at {latest_repair.get('at')}"
                    )
    elif target == "awaiting_acceptance":
        rows = [
            item for item in _passing(run)
            if item.get("gate") in required["deterministic"] | required["judgement"]
        ]
        if any(
            item.get("kind") != (
                "deterministic" if item.get("gate") in required["deterministic"] else "judgement"
            ) for item in rows
        ) or {item.get("gate") for item in rows} != required["deterministic"] | required["judgement"]:
            return "awaiting_acceptance gate is not satisfied"
        if not {
            item.get("id") for item in rows
        } <= cited or not measures_ready(run, profile, cited):
            return "awaiting_acceptance gate is not satisfied"
        if not any(item.get("status") == "pass" for item in run.get("reviews", [])):
            return "awaiting_acceptance gate is not satisfied"
        ladder_error = review_ladder_error(run)
        if ladder_error:
            return ladder_error
    elif target == "accepted":
        gate = run.get("human_gates", {}).get("acceptance", {})
        if gate.get("status") != "approved" or gate.get("evidence") not in cited:
            return "accepted human gate is not satisfied"
    elif target == "observing":
        gate = run.get("human_gates", {}).get("release", {})
        if gate.get("status") != "approved" or gate.get("evidence") not in cited:
            return "observing release gate is not satisfied"
        if run.get("observation", {}).get("status") not in {"active", "pass"}:
            return "observing requires observation status active or pass"
    elif target == "closed" and not observation_ready(run, cited):
        return "closed observation gate is not satisfied"
    return None


def command_checkpoint_set(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        if not isinstance(args.current_slice, str) or not args.current_slice.strip():
            _error(api, "checkpoint.current_slice must be a non-empty string")
        if not isinstance(args.next_action, str) or not args.next_action.strip():
            _error(api, "checkpoint.next_action must be a non-empty string")
        checkpoint = run.get("checkpoint")
        if not isinstance(checkpoint, dict):
            _error(api, "checkpoint must be an object")
        generation = checkpoint.get("generation")
        if isinstance(generation, bool) or not isinstance(generation, int) or generation < 0:
            _error(api, "checkpoint.generation must be a non-negative integer")
        existing_paths = checkpoint.get("artifact_paths")
        if not isinstance(existing_paths, list) or any(
            not isinstance(value, str) or not value for value in existing_paths
        ):
            _error(api, "checkpoint.artifact_paths must be a list of non-empty strings")
        if any(not isinstance(value, str) or not value for value in args.in_flight):
            _error(api, "checkpoint.in_flight must contain non-empty strings")
        additions = []
        for value in args.artifacts:
            target, relative = api["safe_workspace_path"](workspace, value, "checkpoint artifact")
            api["ensure_allowed_artifact_target"](run, workspace, target)
            if not target.is_file():
                _error(api, f"checkpoint artifact does not exist: {value}")
            additions.append(relative)
        for value in existing_paths:
            target, relative = api["safe_workspace_path"](workspace, value, "checkpoint artifact")
            api["ensure_allowed_artifact_target"](run, workspace, target)
            if relative != "RUN.json" and not target.is_file():
                _error(api, f"checkpoint artifact does not exist: {value}")
        checkpoint.update({
            "generation": generation + 1,
            "current_slice": args.current_slice,
            "next_action": args.next_action,
            "in_flight": list(dict.fromkeys(args.in_flight)),
            "artifact_paths": list(dict.fromkeys([*existing_paths, *additions])),
        })
        return {"path": str(_run_dir / "RUN.json"), "generation": generation + 1, "verified": True}
    validation = "checkpoint-compatibility" if getattr(args, "compatibility", False) else "canonical"
    return api["mutate_receipt"](args.run_dir, apply, validation=validation)


def command_transition(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    cited = list(dict.fromkeys(args.evidence_ids))

    def apply(run: dict[str, Any], _run_dir: Path, _workspace: Path) -> dict[str, Any]:
        current = run.get("status")
        side_states = api["SIDE_STATES"]
        history = run.get("state_history")
        previous_row = history[-1] if isinstance(history, list) and history else {}
        if args.target in side_states:
            if current in side_states or current not in api["TRANSITIONS"]:
                _error(api, f"invalid lifecycle transition {current} -> {args.target}")
            if not args.reason or not args.recovery:
                _error(api, "side state requires --reason and --recovery")
            if args.resume_state and args.resume_state != current:
                _error(api, "side state resume-state must equal the interrupted state")
            if args.target == "degraded":
                if not args.degradation_kind:
                    _error(api, "degraded state requires --degradation-kind")
                if args.degradation_kind == "kernel_degraded" and not args.fallback_skill:
                    _error(api, "kernel_degraded requires --fallback-skill")
        elif current in side_states:
            if args.target != previous_row.get("resume_state"):
                _error(api, f"invalid lifecycle recovery {current} -> {args.target}")
        elif args.target not in api["TRANSITIONS"].get(current, set()):
            _error(api, f"invalid lifecycle transition {current} -> {args.target}")
        known = {item.get("id") for item in run.get("evidence", []) if isinstance(item, dict)}
        unknown = set(cited) - known
        if unknown:
            _error(api, "transition references unknown evidence ids: " + ", ".join(sorted(unknown)))
        try:
            proposed = api["timestamp_after"](run)
        except api["ReceiptError"] as exc:
            if "strictly increase" in str(exc):
                _error(api, "transition timestamp must strictly increase")
            raise
        if args.target in {"approved", "reviewing", "awaiting_acceptance", "accepted", "observing", "closed"}:
            error = transition_gate_error(run, args.target, set(cited), api, workspace=_workspace)
            if error:
                _error(api, error)
        if not isinstance(history, list) or not history:
            _error(api, "state_history must be non-empty")
        entry = {
            "state": args.target,
            "at": proposed,
            "evidence_ids": cited,
            "risk_tier": run.get("risk_tier"),
        }
        if args.target in side_states:
            entry.update({
                "reason": args.reason,
                "recovery": args.recovery,
                "resume_state": args.resume_state or current,
            })
            run["degradation"] = {"reason": args.reason, "recovery": args.recovery}
            if args.target == "degraded":
                run["degradation"].update({
                    "kind": args.degradation_kind,
                    "fallback_skill": args.fallback_skill,
                })
        history.append(entry)
        run["status"] = args.target
        run["repair_cycles"] = sum(
            isinstance(item, dict) and item.get("state") == "repairing"
            for item in history
        )
        checkpoint = run.get("checkpoint")
        if not isinstance(checkpoint, dict):
            _error(api, "checkpoint is invalid")
        checkpoint["generation"] = checkpoint.get("generation", 0) + 1
        checkpoint["current_slice"] = args.target.replace("_", "-")
        checkpoint["next_action"] = f"continue from {args.target}"
        if args.target == "closed":
            checkpoint["in_flight"] = []
        return {"from": current, "to": args.target, "at": proposed}
    return api["mutate_receipt"](args.run_dir, apply)


def command_show(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    run_dir, _ = api["resolve_run_dir"](args.run_dir)
    with api["run_lock"](run_dir):
        return {"_raw_receipt": api["load_run"](run_dir)}


def _add_file_artifact(
    run: dict[str, Any], workspace: Path, artifact_id: str, path: str,
    api: dict[str, Any],
) -> tuple[dict[str, Any], str, bytes]:
    api["require_identifier"](artifact_id, "review artifact id")
    target, relative = api["safe_workspace_path"](workspace, path, "review artifact path")
    api["ensure_allowed_source_target"](run, workspace, target)
    api["ensure_allowed_artifact_target"](run, workspace, target)
    raw = target.read_bytes() if target.is_file() else b""
    if not raw:
        _error(api, f"review artifact must be existing and non-empty: {path}")
    digest = api["digest_bytes"](raw)
    existing = [
        item for item in run.get("artifacts", [])
        if isinstance(item, dict) and item.get("id") == artifact_id
    ]
    if existing:
        if existing[0].get("path") != relative or existing[0].get("digest") != digest:
            _error(api, f"review artifact id conflicts with live bytes: {artifact_id}")
        return existing[0], relative, raw
    item = {
        "id": artifact_id, "path": relative,
        "media_type": "application/json" if target.suffix == ".json" else "text/markdown",
        "artifact_type": "evidence", "digest": digest, "class": "evidence",
        "owner": "delivery-chair", "retention": "risk-policy",
    }
    run["artifacts"].append(item)
    return item, relative, raw


def command_review_add(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    review_id = api["require_identifier"](args.review_id, "review id")
    if not args.lenses or any(not lens for lens in args.lenses):
        _error(api, "review requires at least one non-empty lens")
    if args.status != "pass" and not args.reason:
        _error(api, f"{args.status} review requires --reason")
    if args.status == "pass" and (
        not args.artifact or not args.route_receipt or not args.model or not args.reviewer_id
    ):
        _error(api, "passing review requires artifact, route receipt, reviewer id and model")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        if any(
            isinstance(item, dict) and item.get("id") == review_id
            for item in [*run.get("reviews", []), *run.get("evidence", [])]
        ):
            _error(api, f"review id already exists: {review_id}")
        if not args.adapter:
            _error(api, "review adapter must be non-empty")
        if not args.provider_family:
            _error(api, "review provider-family must be non-empty")
        if args.role == "distinct-family" and args.provider_family in api["CHAIR_FAMILIES"]:
            _error(api, "distinct-family review must use a non-primary family")
        if args.status != "pass":
            run["reviews"].append({
                "id": review_id, "role": args.role, "provider_family": args.provider_family,
                "adapter": args.adapter, "model": args.model or "", "reviewer_id": args.reviewer_id or "",
                "independent_of_authorship": True, "lenses": list(dict.fromkeys(args.lenses)),
                "status": args.status, "evidence_id": "", "reason": args.reason,
                "route_receipt_digest": "",
            })
            return {"review_id": review_id, "evidence_id": ""}
        review_artifact, review_path, _review_raw = _add_file_artifact(
            run, workspace, f"{review_id}.artifact", args.artifact, api,
        )
        route_artifact, route_path, route_raw = _add_file_artifact(
            run, workspace, f"{review_id}.route", args.route_receipt, api,
        )
        try:
            route = json.loads(route_raw)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            _error(api, f"route receipt must be readable JSON: {exc}")
        if (
            not isinstance(route, dict) or route.get("status") != "ok"
            or route.get("adapter") != args.adapter
            or route.get("reviewer_id") != args.reviewer_id
            or route.get("resolved_model", route.get("model")) != args.model
            or route.get("model_family") != args.provider_family
            or route.get("provider_assurance") not in CERTIFYING_PROVIDER_ASSURANCE
        ):
            _error(api, "route receipt identity does not match review lineage")
        if args.role == "other-primary" and route.get("cross_family") is not True:
            _error(api, "other-primary review requires a cross-family route receipt")
        recorded = api["timestamp_after"](run)
        profile = profile_requirements(run, api["PROFILE_PATH"], api["ReceiptError"])
        required = required_profile_evidence(run, profile, {
            item["id"]: item for item in run.get("artifacts", [])
            if isinstance(item, dict) and isinstance(item.get("id"), str)
        })
        review_gate = next(iter(sorted(required["judgement"])), "code-review")
        lineage = {
            "adapter": args.adapter, "provider_family": args.provider_family, "model": args.model,
        }
        run["evidence"].append({
            "id": review_id, "kind": "judgement", "gate": review_gate, "status": "pass",
            "method": f"independent review artifact by {args.reviewer_id}",
            "artifact_id": review_artifact["id"], "source_paths": [review_path, route_path],
            "model_lineage": lineage, "reviewer_id": args.reviewer_id,
            "review_artifact_digest": review_artifact["digest"],
            "route_receipt": {"path": route_path, "digest": route_artifact["digest"]},
            "recorded_at": recorded,
        })
        run["reviews"].append({
            "id": review_id, "role": args.role, "provider_family": args.provider_family,
            "adapter": args.adapter, "model": args.model, "reviewer_id": args.reviewer_id,
            "independent_of_authorship": True, "lenses": list(dict.fromkeys(args.lenses)),
            "status": "pass", "evidence_id": review_id, "reason": "",
            "route_receipt_digest": route_artifact["digest"],
        })
        return {"review_id": review_id, "evidence_id": review_id}
    return api["mutate_receipt"](args.run_dir, apply)


def _evidence_references(run: dict[str, Any], evidence_id: str) -> list[str]:
    references = []

    def exact_reference(value: Any) -> bool:
        if isinstance(value, dict):
            for key, child in value.items():
                if key in {"evidence", "evidence_id"} and child == evidence_id:
                    return True
                if key == "evidence_ids" and isinstance(child, list) and evidence_id in child:
                    return True
                if exact_reference(child):
                    return True
        elif isinstance(value, list):
            return any(exact_reference(child) for child in value)
        return False

    for index, row in enumerate(run.get("state_history", [])):
        if isinstance(row, dict) and evidence_id in row.get("evidence_ids", []):
            references.append(f"state_history[{index}]")
    for field, value in run.items():
        if field in {"evidence", "reviews", "state_history"}:
            continue
        if exact_reference(value):
            references.append(field)
    for index, row in enumerate(run.get("reviews", [])):
        if isinstance(row, dict) and evidence_id in {row.get("evidence_id"), row.get("id")}:
            references.append(f"reviews[{index}]")
    return references


def command_evidence_remove(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    run_dir, receipt = api["receipt_path"](args.run_dir)
    workspace = run_dir.parent.parent.resolve()
    with api["run_lock"](run_dir):
        run = api["load_run"](run_dir)
        api["ensure_immutable_risk"](run, workspace)
        api["ensure_mutable"](run)
        matches = [
            row for row in run.get("evidence", [])
            if isinstance(row, dict) and row.get("id") == args.evidence_id
        ]
        if len(matches) != 1:
            _error(api, f"unknown evidence id: {args.evidence_id}")
        refs = _evidence_references(run, args.evidence_id)
        if refs:
            _error(api, f"evidence {args.evidence_id} is referenced by {', '.join(refs)}")
        removed = matches[0]
        run["evidence"].remove(removed)
        if removed.get("kind") != "deterministic":
            run["updated_at"] = api["timestamp_after"](run)
            api["write_json_atomic"](receipt, run)
            return {"evidence_id": args.evidence_id, "removed": True}

        artifact, original = api["bundle_artifact"](run, removed["artifact_id"], workspace)
        bundle_artifact_id = _next_bundle_version(run, removed["artifact_id"], api)
        versioned = copy.deepcopy(artifact)
        versioned["id"] = bundle_artifact_id
        run["artifacts"].append(versioned)
        for row in run["evidence"]:
            if isinstance(row, dict) and row.get("kind") == "deterministic" and row.get("artifact_id") == removed["artifact_id"]:
                row["artifact_id"] = bundle_artifact_id
        bundle_raw = api["bundle_bytes"](run, bundle_artifact_id)
        digest = api["digest_bytes"](bundle_raw)
        target = api["hashed_bundle_path"](original, bundle_artifact_id, digest)
        versioned["path"] = target.relative_to(workspace).as_posix()
        versioned["digest"] = digest
        for row in run["evidence"]:
            if (
                isinstance(row, dict) and row.get("kind") == "deterministic"
                and row.get("artifact_id") == bundle_artifact_id
            ):
                row["result"]["receipt_digest"] = digest
        run["updated_at"] = api["timestamp_after"](run)
        api["publish_bundle_and_receipt"](receipt, run, target, bundle_raw)
    return {"evidence_id": args.evidence_id, "removed": True, "artifact_id": bundle_artifact_id, "receipt_digest": digest}


def command_evidence_observation(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    if not math.isfinite(args.measured_value):
        _error(api, "observation measured value must be finite")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        api["ensure_new_evidence_id"](run, args.evidence_id)
        artifact = api["find_artifact"](run, args.artifact_id)
        target, relative = api["safe_workspace_path"](workspace, artifact.get("path", ""), "observation artifact")
        raw = target.read_bytes() if target.is_file() else b""
        if not raw or artifact.get("digest") != api["digest_bytes"](raw):
            _error(api, "observation evidence artifact must match existing non-empty bytes")
        try:
            content = json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError):
            content = None
        if not isinstance(content, dict) or content.get("gate") != args.gate or content.get("measured_value") != args.measured_value:
            _error(api, f"observation evidence artifact does not corroborate measured value {args.measured_value:g}")
        sources = []
        for source in args.sources:
            source_target, source_relative = api["safe_workspace_path"](workspace, source, "observation source")
            api["ensure_allowed_source_target"](run, workspace, source_target)
            if not source_target.exists():
                _error(api, f"observation source does not exist: {source}")
            sources.append(source_relative)
        run["evidence"].append({
            "id": args.evidence_id,
            "kind": "observation",
            "gate": args.gate,
            "status": "pass",
            "method": "recorded observation measurement",
            "artifact_id": args.artifact_id,
            "source_paths": list(dict.fromkeys(sources)),
            "observed_at": api["timestamp_after"](run),
            "measured_value": args.measured_value,
        })
        return {"evidence_id": args.evidence_id, "status": "pass"}
    return api["mutate_receipt"](args.run_dir, apply)
