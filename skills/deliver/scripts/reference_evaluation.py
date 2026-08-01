#!/usr/bin/env python3
"""Materialise hash-honest delivery reference artifacts for contract tests."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import subprocess
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[3]
SKILLS_ROOT = Path(__file__).resolve().parents[2]


def _materialisation_target(
    root: Path, value: str, field: str, *, allow_existing: bool = False,
) -> Path:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{field} must be a non-empty workspace-relative path")
    relative = Path(value)
    if relative.is_absolute() or ".." in relative.parts:
        raise ValueError(f"{field} must be workspace-relative")
    workspace = root.resolve()
    current = workspace
    for index, part in enumerate(relative.parts):
        current /= part
        if current.is_symlink():
            raise ValueError(f"{field} must not traverse a symlink")
        if index < len(relative.parts) - 1 and current.exists() and not current.is_dir():
            raise ValueError(f"{field} parent must be a directory")
    try:
        current.resolve().relative_to(workspace)
    except ValueError as exc:
        raise ValueError(f"{field} escapes the workspace") from exc
    if not allow_existing and (current.exists() or current.is_symlink()):
        raise ValueError(f"{field} target must not already exist")
    return current


class _MaterialisationWrites:
    def __init__(self, workspace_root: Path) -> None:
        self.workspace_root = workspace_root.resolve()
        self.created: list[Path] = []

    def _open_parent(self, target: Path) -> tuple[int, str]:
        relative = target.relative_to(self.workspace_root)
        if not relative.parts:
            raise ValueError("materialisation target must be a file below the workspace")
        directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0)
        if hasattr(os, "O_NOFOLLOW"):
            directory_flags |= os.O_NOFOLLOW
        parent_fd = os.open(self.workspace_root, directory_flags)
        try:
            for part in relative.parts[:-1]:
                try:
                    next_fd = os.open(part, directory_flags, dir_fd=parent_fd)
                except FileNotFoundError:
                    try:
                        os.mkdir(part, 0o755, dir_fd=parent_fd)
                    except FileExistsError:
                        pass
                    next_fd = os.open(part, directory_flags, dir_fd=parent_fd)
                os.close(parent_fd)
                parent_fd = next_fd
        except BaseException:
            os.close(parent_fd)
            raise
        return parent_fd, relative.parts[-1]

    def write(self, target: Path, payload: bytes) -> None:
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        parent_fd, name = self._open_parent(target)
        try:
            descriptor = os.open(name, flags, 0o644, dir_fd=parent_fd)
            self.created.append(target)
            try:
                with os.fdopen(descriptor, "wb") as stream:
                    stream.write(payload)
            except BaseException:
                try:
                    os.unlink(name, dir_fd=parent_fd)
                except FileNotFoundError:
                    pass
                raise
        finally:
            os.close(parent_fd)

    def rollback(self) -> None:
        for target in reversed(self.created):
            try:
                parent_fd, name = self._open_parent(target)
            except OSError:
                continue
            try:
                os.unlink(name, dir_fd=parent_fd)
            except FileNotFoundError:
                pass
            finally:
                os.close(parent_fd)


def _reference_materialisation_targets(
    run: dict[str, Any], workspace_root: Path, root: Path, *,
    evaluation_repetitions: int, evaluation_sample_size: int,
    skills_root: Path,
) -> None:
    """Validate caller-selected targets before the first materialisation write."""
    reserved: dict[Path, str] = {}

    def reserve(value: str, field: str, root: Path = workspace_root) -> Path:
        target = _materialisation_target(root, value, field)
        previous = reserved.get(target)
        if previous is not None:
            if "source path" in field and "source path" in previous:
                return target
            raise ValueError(f"{field} collides with {previous}")
        reserved[target] = field
        return target

    generated_artifact_ids = {
        run["design"]["artifact_id"], "evidence-bundle", "other-primary-route",
        *(binding["evaluation_artifact_id"] for binding in run["assurance"]["evaluations"]),
    }
    for artifact in run["artifacts"]:
        target = _materialisation_target(
            workspace_root, artifact["path"], f"artifact {artifact['id']} path",
            allow_existing=True,
        )
        if target.exists():
            if artifact["id"] in generated_artifact_ids or not target.is_file():
                raise ValueError(f"artifact {artifact['id']} target must not already exist")
            if target.stat().st_nlink != 1:
                raise ValueError(f"artifact {artifact['id']} target must not be a hardlink")
            actual = "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest()
            if actual != artifact.get("digest"):
                raise ValueError(f"artifact {artifact['id']} target digest does not match")
            previous = reserved.get(target)
            if previous is not None:
                raise ValueError(f"artifact {artifact['id']} path collides with {previous}")
            reserved[target] = f"artifact {artifact['id']} path"
            continue
        previous = reserved.get(target)
        if previous is not None:
            raise ValueError(f"artifact {artifact['id']} path collides with {previous}")
        reserved[target] = f"artifact {artifact['id']} path"

    for item in run["evidence"]:
        if item.get("kind") != "deterministic" or item.get("artifact_id") != "evidence-bundle":
            continue
        report_ref = item.get("result", {}).get("gate_report", {})
        report_path = report_ref.get("path") if isinstance(report_ref, dict) else None
        report_path = report_path or f"evidence/{item['id']}-gate-report.json"
        for path in item.get("source_paths", []):
            if path != report_path:
                reserve(path, f"evidence {item['id']} source path")
        reserve(report_path, f"evidence {item['id']} gate report path")

    evidence_by_id = {item.get("id"): item for item in run.get("evidence", [])}
    artifacts_by_id = {item.get("id"): item for item in run.get("artifacts", [])}
    route_targets: set[Path] = set()
    for review in run.get("reviews", []):
        if review.get("role") != "other-primary":
            continue
        linked = evidence_by_id.get(review.get("evidence_id"))
        route = linked.get("route_receipt") if isinstance(linked, dict) else None
        if not isinstance(route, dict) or not isinstance(route.get("path"), str):
            continue
        route_target = _materialisation_target(
            workspace_root, route["path"], "other-primary route path",
        )
        if route_target in route_targets:
            raise ValueError("duplicate other-primary route path")
        route_targets.add(route_target)
        route_artifact = next(
            (item for item in artifacts_by_id.values() if item.get("path") == route["path"]),
            None,
        )
        if route_artifact is not None and route_artifact.get("id") != "other-primary-route":
            raise ValueError("other-primary route path must use the route artifact")
        if route_artifact is None:
            previous = reserved.get(route_target)
            if previous is not None:
                raise ValueError(f"other-primary route path collides with {previous}")
            reserved[route_target] = "other-primary route path"

    by_id = {item["id"]: item for item in run["artifacts"]}
    for binding in run["assurance"]["evaluations"]:
        if binding["status"] == "planned":
            continue
        evaluation_artifact = by_id[binding["evaluation_artifact_id"]]
        evaluation_target = _materialisation_target(
            workspace_root, evaluation_artifact["path"],
            f"evaluation {binding['evaluation_id']} artifact path",
        )
        evaluation = make_reference_evaluation(
            run["run_id"], root, repetitions=evaluation_repetitions,
            sample_size=evaluation_sample_size, evaluation_id=binding["evaluation_id"],
            status={"complete": "pass", "failed": "fail", "incomplete": "incomplete"}[binding["status"]],
            skills_root=skills_root,
        )
        for artifact in evaluation["artifacts"]:
            reserve(
                artifact["path"],
                f"evaluation {binding['evaluation_id']} nested artifact path",
                evaluation_target.parent,
            )


def _artifact(artifact_id: str, suffix: str = "json") -> dict[str, Any]:
    return {
        "id": artifact_id,
        "path": f"evidence/{artifact_id}.{suffix}",
        "media_type": "application/json" if suffix == "json" else "text/plain",
        "digest": "sha256:" + "f" * 64,
        "owner": "evaluation-chair",
        "retention": "project-policy",
        "data_policy": "synthetic",
    }


def make_reference_evaluation(
    delivery_run_id: str, root: Path = ROOT, *, repetitions: int = 3,
    sample_size: int = 10, evaluation_id: str = "EVAL-REFERENCE",
    status: str = "pass", time_offset_minutes: int = 0,
    skills_root: Path = SKILLS_ROOT,
) -> dict[str, Any]:
    template = skills_root / "evaluate" / "templates" / "EVALUATION.template.json"
    value = json.loads(template.read_text())
    value["evaluation_id"] = evaluation_id
    value["decision"]["enclosing_delivery_run_id"] = delivery_run_id
    value["created_at"] = "2026-07-10T00:02:00Z"
    value["plan"]["frozen_at"] = "2026-07-10T00:02:00Z"
    value["status"] = "pass"
    value["updated_at"] = "2026-07-10T00:05:00Z"
    value["artifacts"].extend([
        _artifact("preflight"),
        _artifact("route"),
        _artifact("input"),
        _artifact("output"),
        _artifact("judgement"),
        _artifact("aggregate"),
    ])
    case_ids = [f"case-{index:03d}" for index in range(1, sample_size + 1)]
    value["plan"]["schedule"].update({
        "cases": [
            {"id": case_id, "category": "quality", "critical": index == 0}
            for index, case_id in enumerate(case_ids)
        ],
        "repetitions": repetitions,
        "seeds": [101 * index for index in range(1, repetitions + 1)],
        "shards": [{"id": "all", "case_ids": case_ids}],
    })
    value["preflight"] = [{
        "id": "fixture-schema",
        "status": "pass",
        "started_at": "2026-07-10T00:03:00Z",
        "completed_at": "2026-07-10T00:03:05Z",
        "evidence_artifact_id": "preflight",
        "exit_code": 0,
        "reason": "",
    }]
    attempts: list[dict[str, Any]] = []
    case_results: list[dict[str, Any]] = []
    judgements: list[dict[str, Any]] = []
    for arm_id, score in (("candidate", 0.9), ("control", 0.85)):
        for repetition in range(1, repetitions + 1):
            attempt_id = f"attempt-{arm_id}-{repetition}"
            arm = next(item for item in value["plan"]["arms"] if item["id"] == arm_id)
            manifest = next(item for item in value["artifacts"] if item["id"] == arm["manifest_artifact_id"])
            attempts.append({
                "id": attempt_id,
                "arm_id": arm_id,
                "family": "synthetic",
                "repetition": repetition,
                "seed": value["plan"]["schedule"]["seeds"][repetition - 1],
                "shard_id": "all",
                "status": "success",
                "started_at": f"2026-07-10T00:03:{repetition * 10:02d}Z",
                "completed_at": f"2026-07-10T00:03:{repetition * 10 + 5:02d}Z",
                "retry_of": "",
                "reason": "",
                "plan_digest": value["plan"]["digest"],
                "shared_runtime_digest": value["plan"]["shared_runtime_digest"],
                "arm_manifest_digest": manifest["digest"],
                "arm_configuration_digest": arm["configuration_digest"],
                "route_receipt_artifact_id": "route",
                "input_artifact_id": "input",
                "output_artifact_id": "output",
                "lineage": {
                    "adapter": "synthetic-runner",
                    "adapter_version": "1",
                    "endpoint_provider": "local",
                    "provider_family": "synthetic",
                    "requested_model": "model-v1",
                    "actual_model": "model-v1",
                    "requested_effort": "standard",
                    "effective_effort": "standard",
                    "capability_source": "frozen route receipt",
                    "session_id": attempt_id,
                    "substitution_reason": "",
                },
                "usage": {"unavailable_reason": "synthetic runner"},
            })
            for case_id in case_ids:
                case_results.append({
                    "attempt_id": attempt_id,
                    "case_id": case_id,
                    "status": "pass",
                    "scores": {"quality": score},
                    "evidence_artifact_id": "output",
                    "evidence_unavailable_reason": "",
                    "reason": "",
                })
                judgements.append({
                    "id": f"judgement-{arm_id}-{repetition}-{case_id}",
                    "grader_id": "ground-truth",
                    "attempt_id": attempt_id,
                    "case_id": case_id,
                    "outcome": "pass",
                    "scores": {"quality": score},
                    "evidence_artifact_id": "judgement",
                })
    value["attempts"] = attempts
    value["case_results"] = case_results
    value["graders"] = [{
        "id": "ground-truth",
        "type": "ground-truth",
        "rubric_artifact_id": "rubric",
        "independent_of_generators": True,
        "blinded": True,
        "conflict": "none",
        "started_at": "2026-07-10T00:04:00Z",
        "completed_at": "2026-07-10T00:04:20Z",
        "input_artifact_id": "output",
        "output_artifact_id": "judgement",
        "usage": {"unavailable_reason": "deterministic ground-truth grader"},
        "lineage": {
            "adapter": "fixture-checker",
            "adapter_version": "1",
            "endpoint_provider": "local",
            "provider_family": "ground-truth",
            "requested_model": "rules-v1",
            "actual_model": "rules-v1",
            "requested_effort": "not-applicable",
            "effective_effort": "not-applicable",
            "capability_source": "pinned local checker",
            "session_id": "grader-ground-truth",
            "substitution_reason": "",
            "route_receipt_artifact_id": "",
        },
    }]
    value["judgements"] = judgements
    value["adjudications"] = []
    planned_case_rows = 2 * repetitions * sample_size
    planned_attempts = 2 * repetitions
    candidate_rows = repetitions * sample_size
    value["results"] = {
        "accounting": {
            "planned": planned_case_rows, "passed": planned_case_rows,
            "failed": 0, "omitted": 0,
            "skipped": 0, "excluded": 0, "timed_out": 0, "invalid": 0,
            "tool_errors": 0,
        },
        "attempt_accounting": {
            "planned": planned_attempts, "base_planned": planned_attempts,
            "retries": 0, "succeeded": planned_attempts,
            "timed_out": 0, "invalid_output": 0, "tool_errors": 0,
            "skipped": 0, "excluded": 0,
        },
        "metrics": [{
            "name": "quality",
            "target_arm_id": "candidate",
            "numerator": 0.9 * candidate_rows,
            "denominator": candidate_rows,
            "value": 0.9,
            "comparisons": [{
                "arm_id": "control",
                "numerator": 0.85 * candidate_rows,
                "denominator": candidate_rows,
                "value": 0.85,
                "delta": 0.05,
                "passed": True,
            }],
            "passed": True,
            "evidence_artifact_id": "aggregate",
        }],
        "failure_artifact_ids": [],
    }
    value["conclusion"] = {
        "status": "pass",
        "machine_only": True,
        "passed_gates": ["quality"],
        "failed_gates": [],
        "limitations": ["synthetic reference fixture"],
        "evidence_artifact_ids": ["aggregate"],
    }
    if status in {"fail", "incomplete"}:
        value["preflight"][0].update({"status": "fail", "exit_code": 1})
        for attempt in value["attempts"]:
            attempt.update({
                "status": "skipped", "output_artifact_id": "",
                "reason": "deterministic preflight failed",
            })
            attempt["lineage"].update({"actual_model": "", "effective_effort": ""})
        for result in value["case_results"]:
            result.update({
                "status": "skipped", "scores": {},
                "reason": "deterministic preflight failed",
                "evidence_artifact_id": "",
                "evidence_unavailable_reason": "generator did not run",
            })
        value["graders"] = []
        value["judgements"] = []
        value["results"]["accounting"].update({
            "passed": 0, "skipped": planned_case_rows,
        })
        value["results"]["attempt_accounting"].update({
            "succeeded": 0, "skipped": planned_attempts,
        })
        metric = value["results"]["metrics"][0]
        metric.update({"numerator": 0.0, "value": 0.0, "passed": False})
        metric["comparisons"][0].update({
            "numerator": 0.0, "value": 0.0, "delta": 0.0, "passed": True,
        })
        value["status"] = status
        value["conclusion"].update({
            "status": status, "passed_gates": [],
            "failed_gates": ["fixture-schema"],
            "evidence_artifact_ids": ["preflight"],
        })
    elif status != "pass":
        raise ValueError("reference evaluation status must be pass, fail or incomplete")
    if time_offset_minutes:
        _shift_timestamps(value, timedelta(minutes=time_offset_minutes))
    return value


def _shift_timestamps(value: Any, delta: timedelta) -> Any:
    if isinstance(value, dict):
        for key, child in value.items():
            value[key] = _shift_timestamps(child, delta)
    elif isinstance(value, list):
        for index, child in enumerate(value):
            value[index] = _shift_timestamps(child, delta)
    elif isinstance(value, str) and value.endswith("Z"):
        try:
            parsed = datetime.fromisoformat(value[:-1] + "+00:00")
        except ValueError:
            return value
        return (parsed + delta).isoformat().replace("+00:00", "Z")
    return value


def _materialise_evaluation(
    value: dict[str, Any], root: Path, writes: _MaterialisationWrites,
) -> None:
    targets = []
    reserved: set[Path] = set()
    for item in value["artifacts"]:
        target = _materialisation_target(
            root, item["path"], f"evaluation artifact {item['id']} path",
        )
        if target in reserved:
            raise ValueError(f"evaluation artifact {item['id']} path collides with another artifact")
        reserved.add(target)
        targets.append(target)
    for item in value["artifacts"]:
        target = targets.pop(0)
        if item["id"] == value["plan"]["artifact_id"]:
            frozen = {
                key: part for key, part in value["plan"].items()
                if key not in {"artifact_id", "digest"}
            }
            payload = (json.dumps(frozen, sort_keys=True, separators=(",", ":")) + "\n").encode()
        else:
            payload = f"artifact:{item['id']}\n".encode()
        writes.write(target, payload)
        item["digest"] = "sha256:" + hashlib.sha256(payload).hexdigest()
    by_id = {item["id"]: item for item in value["artifacts"]}
    value["plan"]["digest"] = by_id[value["plan"]["artifact_id"]]["digest"]
    arms = {item["id"]: item for item in value["plan"]["arms"]}
    for attempt in value["attempts"]:
        arm = arms[attempt["arm_id"]]
        attempt["plan_digest"] = value["plan"]["digest"]
        attempt["arm_manifest_digest"] = by_id[arm["manifest_artifact_id"]]["digest"]


def materialise_evaluation_binding(
    run: dict[str, Any], workspace_root: Path, root: Path = ROOT, *,
    binding_index: int = 0, repetitions: int = 3, sample_size: int = 10,
    time_offset_minutes: int = 0, skills_root: Path = SKILLS_ROOT,
    receipt_identity: str = "RUN.json", _materialise_bundle: bool = True,
    _writes: _MaterialisationWrites | None = None,
) -> dict[str, Any]:
    """Materialise one non-planned assurance binding and return its receipt."""
    writes = _writes or _MaterialisationWrites(workspace_root)
    owns_writes = _writes is None
    if _materialise_bundle:
        _reference_materialisation_targets(
            run, workspace_root, root,
            evaluation_repetitions=repetitions,
            evaluation_sample_size=sample_size,
            skills_root=skills_root,
        )
    binding = run["assurance"]["evaluations"][binding_index]
    receipt_status = {
        "complete": "pass", "failed": "fail", "incomplete": "incomplete",
    }.get(binding["status"])
    if receipt_status is None:
        raise ValueError("only complete or terminal evaluation bindings have artifacts")
    by_id = {item["id"]: item for item in run["artifacts"]}
    evaluation_artifact = by_id[binding["evaluation_artifact_id"]]
    evaluation = make_reference_evaluation(
        run["run_id"], root, repetitions=repetitions, sample_size=sample_size,
        evaluation_id=binding["evaluation_id"], status=receipt_status,
        time_offset_minutes=time_offset_minutes, skills_root=skills_root,
    )
    target = _materialisation_target(
        workspace_root, evaluation_artifact["path"],
        f"evaluation {binding['evaluation_id']} artifact path",
    )
    try:
        _materialise_evaluation(evaluation, target.parent, writes)
        payload = (json.dumps(evaluation, indent=2) + "\n").encode()
        writes.write(target, payload)
        digest = "sha256:" + hashlib.sha256(payload).hexdigest()
        evaluation_artifact["digest"] = digest
        binding.update({
            "evaluation_digest": digest,
            "plan_digest": evaluation["plan"]["digest"],
        })
        linked = next(
            (item for item in run["evidence"] if item["id"] == binding["evidence_id"]),
            None,
        )
        if linked and linked.get("kind") == "deterministic":
            linked["result"]["receipt_digest"] = digest
        evidence_bundle = by_id.get("evidence-bundle")
        if evidence_bundle and _materialise_bundle:
            _materialise_deterministic_evidence_bundle(
                run, evidence_bundle, workspace_root, writes=writes,
                receipt_identity=receipt_identity,
            )
        if linked and linked.get("kind") == "deterministic":
            _materialise_deterministic_result(
                run, linked, workspace_root, writes=writes,
                receipt_identity=receipt_identity,
            )
            linked["result"]["receipt_digest"] = digest
        return evaluation
    except BaseException:
        if owns_writes:
            writes.rollback()
        raise


def _write_delivery_artifact(
    artifact: dict[str, Any], workspace_root: Path, payload: bytes,
    writes: _MaterialisationWrites,
) -> None:
    target = _materialisation_target(
        workspace_root, artifact["path"], f"artifact {artifact['id']} path",
    )
    writes.write(target, payload)
    artifact["digest"] = "sha256:" + hashlib.sha256(payload).hexdigest()


def _git_identity(workspace_root: Path) -> dict[str, Any]:
    """Materialise the real Git identity when the reference runs in a Git root."""
    git_environment = os.environ.copy()
    for name in (
        "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR",
    ):
        git_environment.pop(name, None)
    try:
        root = subprocess.check_output(
            ["git", "-C", str(workspace_root), "rev-parse", "--show-toplevel"],
            text=True, stderr=subprocess.DEVNULL, env=git_environment,
        ).strip()
        head = subprocess.check_output(
            ["git", "-C", str(workspace_root), "rev-parse", "HEAD"],
            text=True, stderr=subprocess.DEVNULL, env=git_environment,
        ).strip()
    except (OSError, subprocess.CalledProcessError):
        return {"available": False, "reason": "reference fixture"}
    if Path(root).resolve() != workspace_root.resolve() or not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", head):
        return {"available": False, "reason": "reference fixture"}
    return {"available": True, "root": str(workspace_root.resolve()), "head": head}


def _materialise_deterministic_result(
    run: dict[str, Any], item: dict[str, Any], workspace_root: Path, *,
    writes: _MaterialisationWrites,
    receipt_identity: str = "RUN.json",
) -> None:
    """Give synthetic deterministic rows the same bounded observation contract."""
    started = item.setdefault("started_at", "2026-07-10T00:00:00Z")
    finished = item.setdefault("finished_at", "2026-07-10T00:00:01Z")
    result = item.setdefault("result", {})
    result.setdefault("exit_code", 0)
    empty_b64 = base64.b64encode(b"").decode("ascii")
    git_identity = _git_identity(workspace_root)
    argv = result.setdefault("argv", ["reference-check"])
    report_ref = result.get("gate_report")
    report_path = (
        report_ref.get("path")
        if isinstance(report_ref, dict) and isinstance(report_ref.get("path"), str)
        else f"evidence/{item['id']}-gate-report.json"
    )
    if report_path not in item.setdefault("source_paths", []):
        item["source_paths"].append(report_path)
    report = {
        "schema_version": 1,
        "contract": "delivery-gate-report",
        "gate": item["gate"],
        "argv": argv,
        "scope": "full",
        "counts": {"collected": 1, "passed": 1, "failed": 0, "skipped": 0, "expected_collected": 1},
        "baseline": {"kind": "structured-runner", "expected_collected": 1},
    }
    report_raw = (json.dumps(report, sort_keys=True) + "\n").encode()
    report_target = _materialisation_target(
        workspace_root, report_path, f"evidence {item['id']} gate report path",
    )
    writes.write(report_target, report_raw)
    environment_identity = {
        "platform": {"system": "reference", "release": "reference", "machine": "reference"},
        "python": {"executable": "reference", "version": "reference"},
        "variables": {name: "reference" for name in ("PATH", "VIRTUAL_ENV", "CONDA_PREFIX", "PYTHONHOME", "NODE_PATH")},
    }
    environment_identity["digest"] = "sha256:" + hashlib.sha256(
        json.dumps(environment_identity, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()
    source_digests = [
        {
            "path": path,
            "digest": "sha256:" + hashlib.sha256((workspace_root / path).read_bytes()).hexdigest(),
            "bytes": len((workspace_root / path).read_bytes()),
        }
        for path in item.get("source_paths", [])
    ]
    result.update({
        "argv": argv,
        "gate_identity": {"id": item["gate"], "argv": argv, "scope": "full"},
        "counts": {"scope": "full", "collected": 1, "passed": 1, "failed": 0, "skipped": 0, "expected_collected": 1},
        "gate_report": {
            "path": report_path,
            "digest": "sha256:" + hashlib.sha256(report_raw).hexdigest(),
            "baseline": {"kind": "structured-runner", "expected_collected": 1},
        },
        "environment": environment_identity,
        "cwd": str(workspace_root.resolve()),
        "run_identity": {
            "run_id": run["run_id"],
            "receipt": receipt_identity,
        },
        "source_digests": source_digests,
        "source_digests_after": source_digests,
        "git": {"before": git_identity, "after": git_identity},
        "stdout": {"digest": "sha256:" + hashlib.sha256(b"").hexdigest(), "bytes": 0, "retained_bytes": 0, "truncated": False, "complete": True, "captured_b64": empty_b64, "retained_b64": empty_b64},
        "stderr": {"digest": "sha256:" + hashlib.sha256(b"").hexdigest(), "bytes": 0, "retained_bytes": 0, "truncated": False, "complete": True, "captured_b64": empty_b64, "retained_b64": empty_b64},
        "signal": None,
        "timed_out": False,
        "custody": {
            "status": "posix-process-group-cleanup",
            "cleanup": {"strategy": "posix-process-group-cleanup", "term_sent": False, "kill_sent": False, "grace_seconds": 0.1},
            "unsupported": "Commands that daemonise or call setsid are unsupported.",
        },
        "started_at": started,
        "finished_at": finished,
    })


def _materialise_deterministic_evidence_bundle(
    run: dict[str, Any], artifact: dict[str, Any], workspace_root: Path, *,
    writes: _MaterialisationWrites | None = None,
    receipt_identity: str = "RUN.json",
) -> None:
    """Write a non-self-referential receipt and bind its evidence rows."""
    writes = writes or _MaterialisationWrites(workspace_root)
    for item in run["evidence"]:
        if item.get("kind") == "deterministic" and item.get("artifact_id") == artifact["id"]:
            report_ref = item.get("result", {}).get("gate_report", {})
            report_path = (
                report_ref.get("path")
                if isinstance(report_ref, dict) and isinstance(report_ref.get("path"), str)
                else f"evidence/{item['id']}-gate-report.json"
            )
            if report_path not in item.setdefault("source_paths", []):
                item["source_paths"].append(report_path)
    source_paths = {
        path
        for item in run["evidence"]
        if item.get("kind") == "deterministic" and item.get("artifact_id") == artifact["id"]
        for path in item.get("source_paths", [])
    }
    report_paths = {
        (
            item.get("result", {}).get("gate_report", {}).get("path")
            if isinstance(item.get("result", {}).get("gate_report"), dict)
            else None
        ) or f"evidence/{item['id']}-gate-report.json"
        for item in run["evidence"]
        if item.get("kind") == "deterministic" and item.get("artifact_id") == artifact["id"]
    }
    for path in source_paths:
        if path in report_paths:
            continue
        target = _materialisation_target(
            workspace_root, path, "deterministic evidence source path",
        )
        writes.write(target, b"reference-input\n")
    checks = []
    for item in run["evidence"]:
        if item.get("kind") != "deterministic" or item.get("artifact_id") != artifact["id"]:
            continue
        _materialise_deterministic_result(
            run, item, workspace_root, writes=writes, receipt_identity=receipt_identity,
        )
        result = item["result"]
        checks.append({
            "id": item["id"],
            "gate": item["gate"],
            "status": item["status"],
            "method": item["method"],
            "source_paths": item["source_paths"],
            "started_at": item["started_at"],
            "finished_at": item["finished_at"],
            "result": {key: value for key, value in item["result"].items() if key != "receipt_digest"},
        })
    payload = (json.dumps({
        "schema_version": 1,
        "contract": "deterministic-evidence-bundle",
        "checks": checks,
    }, sort_keys=True) + "\n").encode()
    _write_delivery_artifact(artifact, workspace_root, payload, writes)
    for item in run["evidence"]:
        if item.get("kind") == "deterministic" and item.get("artifact_id") == artifact["id"]:
            item["result"]["receipt_digest"] = artifact["digest"]
    approval_evidence = next(
        (item for item in run["evidence"] if item.get("id") == run["authority"]["evidence"]),
        None,
    )
    if approval_evidence and approval_evidence.get("artifact_id") == artifact["id"]:
        run["authority"]["evidence_digest"] = artifact["digest"]


def _materialise_reference_routes(
    run: dict[str, Any], workspace_root: Path, writes: _MaterialisationWrites,
) -> None:
    reviews = [item for item in run.get("reviews", []) if item.get("role") == "other-primary"]
    evidence_by_id = {item.get("id"): item for item in run.get("evidence", [])}
    artifacts_by_id = {item.get("id"): item for item in run.get("artifacts", [])}
    for review in reviews:
        linked = evidence_by_id.get(review.get("evidence_id"))
        route_artifact = next(
            (item for item in artifacts_by_id.values() if item.get("path") == linked.get("route_receipt", {}).get("path")),
            None,
        ) if isinstance(linked, dict) else None
        if not isinstance(linked, dict) or not isinstance(route_artifact, dict):
            continue
        route = {
            "status": "ok", "cross_family": True, "certification_eligible": True,
            "adapter": review["adapter"], "reviewer_id": review.get("reviewer_id", "reference-anthropic"),
            "model_family": review["provider_family"], "resolved_model": review["model"],
        }
        payload = (json.dumps(route, sort_keys=True) + "\n").encode()
        _write_delivery_artifact(route_artifact, workspace_root, payload, writes)
        linked["route_receipt"]["digest"] = route_artifact["digest"]


def materialise_reference_run(
    run: dict[str, Any], workspace_root: Path, root: Path = ROOT, *,
    evaluation_repetitions: int = 3, evaluation_sample_size: int = 10,
    skills_root: Path = SKILLS_ROOT, receipt_identity: str = "RUN.json",
) -> dict[str, Any]:
    """Write every local reference artifact and replace placeholders with live digests."""
    workspace_root.mkdir(parents=True, exist_ok=True)
    _reference_materialisation_targets(
        run, workspace_root, root,
        evaluation_repetitions=evaluation_repetitions,
        evaluation_sample_size=evaluation_sample_size,
        skills_root=skills_root,
    )
    writes = _MaterialisationWrites(workspace_root)
    try:
        by_id = {item["id"]: item for item in run["artifacts"]}
        canonical_id = run["design"]["artifact_id"]
        _write_delivery_artifact(
            by_id[canonical_id], workspace_root,
            b"approved synthetic reference intent\n", writes,
        )
        run["intent"]["digest"] = by_id[canonical_id]["digest"]
        run["design"]["digest"] = by_id[canonical_id]["digest"]
        _materialise_deterministic_evidence_bundle(
            run, by_id["evidence-bundle"], workspace_root, writes=writes,
            receipt_identity=receipt_identity,
        )

        for index, binding in enumerate(run["assurance"]["evaluations"]):
            if binding["status"] == "planned":
                continue
            materialise_evaluation_binding(
                run, workspace_root, root, binding_index=index,
                repetitions=evaluation_repetitions, sample_size=evaluation_sample_size,
                skills_root=skills_root, receipt_identity=receipt_identity,
                _materialise_bundle=False, _writes=writes,
            )
        _materialise_reference_routes(run, workspace_root, writes)
        return run
    except BaseException:
        writes.rollback()
        raise
