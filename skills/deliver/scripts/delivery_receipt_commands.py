"""Subcommands and review helpers for the flat delivery receipt producer."""

from __future__ import annotations

import argparse
import json
import math
from pathlib import Path
from typing import Any


def profile_judgement_gate(
    profile: str, profile_path: Path, error_type: type[ValueError],
) -> str:
    try:
        registry = json.loads(profile_path.read_text())
        gates = registry["profiles"][profile]["required_evidence"]["judgement"]
    except (OSError, KeyError, TypeError, json.JSONDecodeError) as exc:
        raise error_type(f"delivery profile registry is unreadable: {exc}") from exc
    if not isinstance(gates, list) or not gates or not isinstance(gates[0], str):
        raise error_type(f"profile {profile} has no judgement evidence gate")
    return gates[0]


def add_review_artifact(
    run: dict[str, Any],
    workspace: Path,
    *,
    artifact_id: str,
    path: str,
    api: dict[str, Any],
) -> tuple[dict[str, Any], str, str, bytes]:
    api["require_identifier"](artifact_id, "review artifact id")
    target, relative = api["safe_workspace_path"](
        workspace, path, "review artifact path",
    )
    api["ensure_allowed_source_target"](run, workspace, target)
    api["ensure_allowed_artifact_target"](run, workspace, target)
    raw = target.read_bytes() if target.is_file() else b""
    if not raw:
        raise api["ReceiptError"](
            f"review artifact must be existing and non-empty: {path}"
        )
    digest = api["digest_bytes"](raw)
    existing = [
        item for item in run.get("artifacts", [])
        if isinstance(item, dict) and item.get("id") == artifact_id
    ]
    if existing:
        if existing[0].get("path") != relative or existing[0].get("digest") != digest:
            raise api["ReceiptError"](
                f"review artifact id conflicts with live bytes: {artifact_id}"
            )
        return existing[0], relative, digest, raw
    item = {
        "id": artifact_id,
        "path": relative,
        "media_type": "application/json" if target.suffix == ".json" else "text/markdown",
        "artifact_type": "evidence",
        "digest": digest,
        "class": "evidence",
        "owner": "delivery-chair",
        "retention": "risk-policy",
    }
    run["artifacts"].append(item)
    return item, relative, digest, raw


def command_checkpoint_set(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        checkpoint = run.get("checkpoint")
        if not isinstance(checkpoint, dict):
            raise api["ReceiptError"]("checkpoint must be an object")
        generation = checkpoint.get("generation")
        if (
            isinstance(generation, bool)
            or not isinstance(generation, int)
            or generation < 0
        ):
            raise api["ReceiptError"](
                "checkpoint.generation must be a non-negative integer"
            )
        in_flight = list(dict.fromkeys(args.in_flight))
        if any(not value for value in in_flight):
            raise api["ReceiptError"]("in-flight IDs must be non-empty")
        existing = checkpoint.get("artifact_paths")
        if not isinstance(existing, list):
            raise api["ReceiptError"]("checkpoint.artifact_paths must be a list")
        additions: list[str] = []
        for artifact in args.artifacts:
            target, relative = api["safe_workspace_path"](
                workspace, artifact, "checkpoint artifact",
            )
            if not target.is_file():
                raise api["ReceiptError"](
                    f"checkpoint artifact does not exist: {artifact}"
                )
            additions.append(relative)
        checkpoint.update({
            "generation": generation + 1,
            "current_slice": args.current_slice,
            "next_action": args.next_action,
            "in_flight": in_flight,
            "artifact_paths": list(dict.fromkeys([*existing, *additions])),
        })
        return {"generation": generation + 1}

    return api["mutate_run"](args.run_dir, apply)


def artifact_corroborates_measurement(
    value: Any, gate: str, measured_value: float,
) -> bool:
    if not isinstance(value, dict) or value.get("gate") != gate:
        return False
    recorded = value.get("measured_value")
    return (
        not isinstance(recorded, bool)
        and isinstance(recorded, (int, float))
        and math.isfinite(recorded)
        and recorded == measured_value
    )


def command_evidence_observation(
    args: Any, api: dict[str, Any],
) -> dict[str, Any]:
    evidence_id = api["require_identifier"](args.evidence_id, "evidence id")
    if not math.isfinite(args.measured_value):
        raise api["ReceiptError"]("observation measured value must be finite")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        api["ensure_new_evidence_id"](run, evidence_id)
        artifact = api["find_artifact"](run, args.artifact_id)
        if not artifact.get("path"):
            raise api["ReceiptError"]("observation evidence must use a local artifact")
        target, _relative = api["safe_workspace_path"](
            workspace, artifact["path"], "observation evidence artifact",
        )
        raw = target.read_bytes() if target.is_file() else b""
        if not raw or artifact.get("digest") != api["digest_bytes"](raw):
            raise api["ReceiptError"](
                "observation evidence artifact must match existing non-empty bytes"
            )
        try:
            artifact_content = json.loads(raw)
        except (TypeError, UnicodeDecodeError, json.JSONDecodeError):
            artifact_content = None
        if not artifact_corroborates_measurement(
            artifact_content, args.gate, args.measured_value,
        ):
            raise api["ReceiptError"](
                "observation evidence artifact does not corroborate measured value "
                f"{args.measured_value:g}"
            )
        sources: list[str] = []
        for source in args.sources:
            source_target, relative = api["safe_workspace_path"](
                workspace, source, "observation evidence source",
            )
            api["ensure_allowed_source_target"](run, workspace, source_target)
            if not source_target.exists():
                raise api["ReceiptError"](
                    f"observation evidence source does not exist: {source}"
                )
            sources.append(relative)
        run["evidence"].append({
            "id": evidence_id,
            "kind": "observation",
            "gate": args.gate,
            "status": "pass",
            "method": "recorded observation measurement",
            "artifact_id": args.artifact_id,
            "source_paths": list(dict.fromkeys(sources)),
            "observed_at": api["utc_now"](),
            "measured_value": args.measured_value,
        })
        return {"evidence_id": evidence_id, "status": "pass"}

    return api["mutate_run"](args.run_dir, apply)


def parse_utc(value: Any, field: str, error_type: type[ValueError]):
    if not isinstance(value, str) or not value.endswith("Z"):
        raise error_type(f"{field} must be an ISO UTC timestamp")
    try:
        return api_datetime().fromisoformat(value[:-1])
    except ValueError as exc:
        raise error_type(f"{field} must be an ISO UTC timestamp") from exc


def api_datetime():
    from datetime import datetime

    return datetime


def artifact_map(run: dict[str, Any]) -> dict[str, dict[str, Any]]:
    return {
        item["id"]: item for item in run.get("artifacts", [])
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    }


def command_review_add(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    require_identifier = api["require_identifier"]
    receipt_error = api["ReceiptError"]
    review_id = require_identifier(args.review_id, "review id")
    if not args.adapter:
        raise receipt_error("review requires non-empty --adapter")
    if not args.provider_family:
        raise receipt_error("review requires non-empty --provider-family")
    if args.role == "distinct-family" and args.provider_family in {"openai", "anthropic"}:
        raise receipt_error("distinct-family review must use a non-primary family")
    if args.status == "pass":
        missing = [
            option for option in ("artifact", "route_receipt", "reviewer_id", "model")
            if not getattr(args, option)
        ]
        if missing:
            raise receipt_error(
                "passing review requires " + ", ".join(
                    f"--{option.replace('_', '-')}" for option in missing
                )
            )
        require_identifier(args.reviewer_id, "reviewer-id")
    elif not args.reason:
        raise receipt_error(f"{args.status} review requires --reason")

    def apply(run: dict[str, Any], _run_dir: Path, workspace: Path) -> dict[str, Any]:
        if any(
            isinstance(item, dict) and item.get("id") == review_id
            for item in [*run.get("reviews", []), *run.get("evidence", [])]
        ):
            raise receipt_error(f"review id already exists: {review_id}")
        if not args.lenses or any(not lens for lens in args.lenses):
            raise receipt_error("review requires at least one non-empty lens")
        if args.status != "pass":
            run["reviews"].append({
                "id": review_id, "role": args.role,
                "provider_family": args.provider_family, "adapter": args.adapter,
                "model": args.model or "", "reviewer_id": args.reviewer_id or "",
                "independent_of_authorship": True,
                "lenses": list(dict.fromkeys(args.lenses)), "status": args.status,
                "evidence_id": "", "reason": args.reason,
                "route_receipt_digest": "",
            })
            return {"review_id": review_id, "evidence_id": ""}
        review_artifact_id = f"{review_id}.artifact"
        route_artifact_id = f"{review_id}.route"
        _artifact, review_path, review_digest, _review_raw = api["add_review_artifact"](
            run, workspace, artifact_id=review_artifact_id, path=args.artifact,
        )
        _route, route_path, route_digest, route_raw = api["add_review_artifact"](
            run, workspace, artifact_id=route_artifact_id, path=args.route_receipt,
        )
        try:
            route = json.loads(route_raw)
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise receipt_error(f"route receipt must be readable JSON: {exc}") from exc
        if (
            not isinstance(route, dict) or route.get("status") != "ok"
            or route.get("adapter") != args.adapter
            or route.get("reviewer_id") != args.reviewer_id
            or route.get("resolved_model", route.get("model")) != args.model
            or route.get("model_family") != args.provider_family
            or route.get("certification_eligible") is not True
        ):
            raise receipt_error("route receipt identity does not match review lineage")
        if args.role == "other-primary" and route.get("cross_family") is not True:
            raise receipt_error("other-primary review requires a cross-family route receipt")
        lineage = {
            "adapter": args.adapter, "provider_family": args.provider_family,
            "model": args.model,
        }
        run["evidence"].append({
            "id": review_id, "kind": "judgement",
            "gate": api["profile_judgement_gate"](run["profile"]), "status": "pass",
            "method": f"independent review artifact by {args.reviewer_id}",
            "artifact_id": review_artifact_id, "source_paths": [review_path, route_path],
            "model_lineage": lineage, "reviewer_id": args.reviewer_id,
            "review_artifact_digest": review_digest,
            "route_receipt": {"path": route_path, "digest": route_digest},
            "recorded_at": api["utc_now"](),
        })
        run["reviews"].append({
            "id": review_id, "role": args.role,
            "provider_family": args.provider_family, "adapter": args.adapter,
            "model": args.model, "reviewer_id": args.reviewer_id,
            "independent_of_authorship": True,
            "lenses": list(dict.fromkeys(args.lenses)), "status": "pass",
            "evidence_id": review_id, "reason": "",
            "route_receipt_digest": route_digest,
        })
        return {"review_id": review_id, "evidence_id": review_id}

    return api["mutate_run"](args.run_dir, apply)


def build_parser(api: dict[str, Any]) -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=api["__doc__"])
    subcommands = parser.add_subparsers(dest="command", required=True)
    init = subcommands.add_parser("init")
    for name in ("run-dir", "run-id", "profile", "chair-family", "risk-assessment", "intent", "authority"):
        init.add_argument(f"--{name}", required=True)
    init.add_argument("--fabric-relationships")
    init.add_argument("--risk-tier", choices=api["RISKS"])
    init.add_argument("--risk-override")
    init.set_defaults(handler=api["command_init"])

    bind = subcommands.add_parser("bind")
    bind.add_argument("--run-dir", required=True)
    bind.add_argument("--section", choices=sorted(api["BIND_SECTIONS"]), required=True)
    bind.add_argument("--from", dest="from_json", required=True)
    bind.set_defaults(handler=api["command_bind"])

    artifact = subcommands.add_parser("artifact")
    artifact_commands = artifact.add_subparsers(dest="artifact_command", required=True)
    artifact_add = artifact_commands.add_parser("add")
    artifact_add.add_argument("--run-dir", required=True)
    artifact_add.add_argument("--id", dest="artifact_id", required=True)
    artifact_add.add_argument("--path", required=True)
    artifact_add.add_argument("--class", dest="artifact_class", choices=sorted(api["SAFE_CLASSES"]), required=True)
    artifact_add.add_argument("--media-type", required=True)
    artifact_add.add_argument("--artifact-type", required=True)
    artifact_add.add_argument("--owner", required=True)
    artifact_add.add_argument("--retention", required=True)
    artifact_add.set_defaults(handler=api["command_artifact_add"])

    evidence = subcommands.add_parser("evidence")
    evidence_commands = evidence.add_subparsers(dest="evidence_command", required=True)
    evidence_run = evidence_commands.add_parser("run")
    evidence_run.add_argument("--run-dir", required=True)
    evidence_run.add_argument("--id", dest="evidence_id", required=True)
    evidence_run.add_argument("--gate", required=True)
    evidence_run.add_argument("--artifact-id", required=True)
    evidence_run.add_argument("--source", dest="sources", action="append", required=True)
    evidence_run.add_argument("command_args", nargs=argparse.REMAINDER)
    evidence_run.set_defaults(handler=api["command_evidence_run"])

    evidence_human = evidence_commands.add_parser("human")
    evidence_human.add_argument("--run-dir", required=True)
    evidence_human.add_argument("--id", dest="evidence_id", required=True)
    evidence_human.add_argument("--gate", required=True)
    evidence_human.add_argument("--artifact-id", required=True)
    evidence_human.add_argument("--approver", required=True)
    evidence_human.add_argument("--source", dest="sources", action="append", default=[])
    evidence_human.set_defaults(handler=api["command_evidence_human"])

    observation = evidence_commands.add_parser("observation")
    observation.add_argument("--run-dir", required=True)
    observation.add_argument("--id", dest="evidence_id", required=True)
    observation.add_argument("--gate", required=True)
    observation.add_argument("--artifact-id", required=True)
    observation.add_argument("--measured-value", type=float, required=True)
    observation.add_argument("--source", dest="sources", action="append", required=True)
    observation.set_defaults(handler=lambda args: command_evidence_observation(args, api))

    remove = evidence_commands.add_parser("remove")
    remove.add_argument("--run-dir", required=True)
    remove.add_argument("--id", dest="evidence_id", required=True)
    remove.set_defaults(handler=api["command_evidence_remove"])

    rebuild = evidence_commands.add_parser("rebuild")
    rebuild.add_argument("--run-dir", required=True)
    rebuild.add_argument("--artifact-id", required=True)
    rebuild.set_defaults(handler=api["command_evidence_rebuild"])

    review = subcommands.add_parser("review")
    review_commands = review.add_subparsers(dest="review_command", required=True)
    review_add = review_commands.add_parser("add")
    review_add.add_argument("--run-dir", required=True)
    review_add.add_argument("--id", dest="review_id", required=True)
    review_add.add_argument("--role", choices=("distinct-family", "other-primary", "targeted"), required=True)
    review_add.add_argument("--artifact")
    review_add.add_argument("--route-receipt")
    review_add.add_argument("--reviewer-id")
    review_add.add_argument("--adapter", required=True)
    review_add.add_argument("--provider-family", required=True)
    review_add.add_argument("--model")
    review_add.add_argument("--lens", dest="lenses", action="append", required=True)
    review_add.add_argument("--status", choices=("pass", "failed", "unavailable", "skipped"), default="pass")
    review_add.add_argument("--reason", default="")
    review_add.set_defaults(handler=lambda args: command_review_add(args, api))

    checkpoint = subcommands.add_parser("checkpoint")
    checkpoint_commands = checkpoint.add_subparsers(dest="checkpoint_command", required=True)
    checkpoint_set = checkpoint_commands.add_parser("set")
    checkpoint_set.add_argument("--run-dir", required=True)
    checkpoint_set.add_argument("--current-slice", required=True)
    checkpoint_set.add_argument("--next-action", required=True)
    checkpoint_set.add_argument("--in-flight", action="append", default=[])
    checkpoint_set.add_argument("--artifact", dest="artifacts", action="append", default=[])
    checkpoint_set.set_defaults(handler=lambda args: command_checkpoint_set(args, api))

    repair = subcommands.add_parser("repair")
    repair.add_argument("--run-dir", required=True)
    repair.add_argument("--reason", required=True)
    repair.set_defaults(handler=api["command_repair"])

    show = subcommands.add_parser("show")
    show.add_argument("--run-dir", required=True)
    show.set_defaults(handler=lambda args: command_show(args, api))
    return parser


def command_show(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    run_dir, _workspace = api["resolve_run_dir"](args.run_dir)
    with api["run_lock"](run_dir):
        return {"_raw_receipt": api["load_run"](run_dir)}


def command_repair(args: Any, api: dict[str, Any]) -> dict[str, Any]:
    """Record one repair cycle. The budget by risk tier is enforced by the reader."""
    if not args.reason:
        raise api["ReceiptError"]("repair requires a reason")

    def apply(run: dict[str, Any], _run_dir: Path, _workspace: Path) -> dict[str, Any]:
        cycles = run.get("repair_cycles")
        if isinstance(cycles, bool) or not isinstance(cycles, int) or cycles < 0:
            raise api["ReceiptError"]("repair_cycles must be a non-negative integer")
        run["repair_cycles"] = cycles + 1
        checkpoint = run.get("checkpoint")
        if not isinstance(checkpoint, dict) or not isinstance(
            checkpoint.get("generation"), int,
        ):
            raise api["ReceiptError"]("checkpoint is invalid")
        checkpoint["generation"] += 1
        checkpoint["next_action"] = args.reason
        return {"repair_cycles": run["repair_cycles"]}

    return api["mutate_run"](args.run_dir, apply)
