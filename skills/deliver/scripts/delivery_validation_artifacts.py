"""Artifact validation for delivery receipts."""

from __future__ import annotations

import hashlib
from pathlib import Path
from typing import Any

from delivery_validation_common import (
    Invalid, SAFE_CLASSES, _digest, _inside, _mapping, _safe_path,
    _software_delivery_validator, fail,
)

def _validate_artifacts(
    artifacts: list[Any], *, workspace_root: Path | None, verify_hashes: bool,
    allowed_artifact_paths: list[str], allowed_source_paths: list[str],
    profile: dict[str, Any],
) -> dict[str, dict[str, Any]]:
    by_id: dict[str, dict[str, Any]] = {}
    for index, raw in enumerate(artifacts):
        item = _mapping(raw, f"artifacts[{index}]")
        artifact_id = item.get("id")
        fail(not isinstance(artifact_id, str) or not artifact_id or artifact_id in by_id, f"artifact {index} id is missing or duplicate")
        path = item.get("path")
        uri = item.get("uri")
        path_present = "path" in item
        revision_present = "git_revision" in item
        _software_delivery_validator().validate_git_artifact(
            item, artifact_id, path, uri, workspace_root, allowed_source_paths,
            verify_hashes, _safe_path, _inside, Invalid,
        )
        if path_present:
            clean_path = _safe_path(path, f"artifact {artifact_id}.path")
            fail(not any(_inside(clean_path, scope) for scope in allowed_artifact_paths), f"artifact {artifact_id} is outside authority.allowed_artifact_paths")
        fail(not item.get("media_type"), f"artifact {artifact_id} requires media_type")
        fail(item.get("class") not in SAFE_CLASSES, f"artifact {artifact_id} has invalid class")
        artifact_type = item.get("artifact_type")
        fail(not isinstance(artifact_type, str) or not artifact_type, f"artifact {artifact_id} requires artifact_type")
        if item.get("class") == "canonical":
            fail(artifact_type not in profile["artifact_types"], f"canonical artifact {artifact_id} type is outside the selected profile")
        else:
            expected_type = {"evidence": "evidence", "handoff": "handoff", "scratch": "scratch", "external": "external-reference"}[item["class"]]
            fail(artifact_type != expected_type, f"artifact {artifact_id} type does not match its class")
        fail(not item.get("owner") or not item.get("retention"), f"artifact {artifact_id} requires owner and retention")
        if item.get("class") == "evidence":
            fail(item.get("retention") not in profile["evidence_policy"]["retention"], f"evidence artifact {artifact_id} retention violates the profile policy")
        digest = item.get("digest")
        _software_delivery_validator().validate_integrity_shape(
            item, artifact_id, revision_present, path_present, _digest, fail)
        if path_present and verify_hashes:
            fail(workspace_root is None, "verify_hashes requires workspace_root")
            target = workspace_root / path
            try:
                target.resolve().relative_to(workspace_root.resolve())
            except ValueError as exc:
                raise Invalid(f"artifact {artifact_id} resolves outside workspace_root") from exc
            fail(not target.is_file(), f"artifact {artifact_id} path does not exist")
            actual = "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest()
            fail(actual != digest, f"artifact {artifact_id} digest does not match live bytes")
        by_id[artifact_id] = item
    fail(not by_id, "at least one artifact is required")
    fail(not any(item.get("class") == "canonical" for item in by_id.values()), "profile requires a canonical outcome artifact")
    return by_id

