#!/usr/bin/env python3
"""Plan or perform authority-gated removal of expired delivery-run scratch."""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
from typing import Any


class CleanupError(ValueError):
    pass


# `scripts/lib/roots.py` is the single resolver for the product root (#754).
# The fallback loads that one file when this script is run directly by path and
# the product root is not on `sys.path`: it locates the resolver, it does not
# decide the root, and it leaves import resolution untouched (#755).
try:
    from scripts.lib.roots import product_root
except ModuleNotFoundError:  # pragma: no cover - direct invocation by path
    import importlib.util as _roots_util
    _roots_spec = _roots_util.spec_from_file_location(
        "provenant_roots", Path(__file__).resolve().parents[3] / "scripts" / "lib" / "roots.py"
    )
    _roots_module = _roots_util.module_from_spec(_roots_spec)
    _roots_spec.loader.exec_module(_roots_module)
    product_root = _roots_module.product_root

PRODUCT_ROOT = product_root()
SKILLS_ROOT = Path(__file__).resolve().parents[2]


def _delivery_validator():
    path = SKILLS_ROOT / "deliver" / "scripts" / "validate_delivery.py"
    spec = importlib.util.spec_from_file_location("cleanup_delivery_validator", path)
    if not spec or not spec.loader:
        raise CleanupError("cannot load delivery validator")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _run_shape():
    path = SKILLS_ROOT / "deliver" / "scripts" / "delivery_run_shape.py"
    spec = importlib.util.spec_from_file_location("cleanup_run_shape", path)
    if not spec or not spec.loader:
        raise CleanupError("cannot load the delivery run shape module")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _utc(value: Any, field: str) -> datetime:
    if not isinstance(value, str) or not value.endswith("Z"):
        raise CleanupError(f"{field} must be a UTC timestamp")
    try:
        return datetime.fromisoformat(value[:-1] + "+00:00")
    except ValueError as exc:
        raise CleanupError(f"{field} is invalid") from exc


def cleanup(
    receipt_path: Path,
    *,
    workspace_root: Path | None = None,
    execute: bool = False,
    authorised_by: str = "",
    authority_evidence: str = "",
    approved_plan_sha256: str = "",
    now: datetime | None = None,
    product_root: Path = PRODUCT_ROOT,
) -> dict[str, Any]:
    receipt_path = receipt_path.resolve()
    run_dir = receipt_path.parent
    workspace_root = (workspace_root or run_dir).resolve()
    try:
        run = json.loads(receipt_path.read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise CleanupError(f"RUN receipt is unreadable: {exc}") from exc
    if not isinstance(run, dict) or run.get("schema_version") != 1 or run.get("contract") != "delivery-run" or not isinstance(run.get("artifacts"), list):
        raise CleanupError("cleanup requires a canonical delivery RUN artifact manifest")
    validator = _delivery_validator()
    try:
        validator.validate(
            run, product_root, receipt_dir=run_dir, workspace_root=workspace_root,
        )
    except validator.Invalid as exc:
        raise CleanupError(f"valid terminal delivery receipt required: {exc}") from exc
    if not _run_shape().run_closed(run):
        raise CleanupError(
            "valid terminal delivery receipt required: the run must have an approved "
            "release gate and a settled observation",
        )
    if execute and (not authorised_by or not authority_evidence):
        raise CleanupError("execute requires explicit cleanup authority and evidence")
    now = now or datetime.now(timezone.utc)
    paths: set[str] = set()
    eligible_rows: list[dict[str, Any]] = []
    for index, artifact in enumerate(run["artifacts"]):
        if not isinstance(artifact, dict):
            raise CleanupError(f"artifact {index} must be an object")
        value = artifact.get("path")
        if not isinstance(value, str) or not value:
            continue
        path = Path(value)
        if path.is_absolute() or ".." in path.parts:
            raise CleanupError(f"artifact {index} path must be safe and run-relative")
        relative = path.as_posix()
        if relative in paths:
            raise CleanupError(f"duplicate artifact path: {relative}")
        paths.add(relative)
        if artifact.get("class") != "scratch":
            continue
        if artifact.get("owner") != run.get("run_id") or not artifact.get("retention"):
            raise CleanupError(f"scratch artifact {index} is not owned by this run")
        expires = artifact.get("expires_at")
        if expires and _utc(expires, f"artifact {index}.expires_at") <= now:
            target = workspace_root / path
            try:
                target.resolve().relative_to(run_dir.resolve())
            except ValueError as exc:
                raise CleanupError(f"artifact {index} resolves outside run directory") from exc
            if target.is_symlink():
                raise CleanupError(f"scratch artifact {index} is a symlink")
            if target.is_file():
                digest = "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest()
                if artifact.get("digest") != digest:
                    raise CleanupError(f"scratch artifact {index} digest does not match current bytes")
                eligible_rows.append({"artifact_id": artifact.get("id"), "path": relative, "digest": digest, "size": target.stat().st_size})
    plan_sha256 = "sha256:" + hashlib.sha256(json.dumps({"run_id": run.get("run_id"), "eligible": eligible_rows}, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    removed: list[str] = []
    failure = ""
    if execute:
        if approved_plan_sha256 != plan_sha256:
            raise CleanupError("approved plan digest does not match the current cleanup plan")
        for row in eligible_rows:
            relative = row["path"]
            try:
                (workspace_root / relative).unlink()
            except OSError as exc:
                failure = f"failed after removing {len(removed)} artifact(s): {exc}"
                break
            else:
                removed.append(relative)
    return {
        "schema_version": 1,
        "status": "fail" if failure else "pass",
        "error": failure,
        "mode": "execute" if execute else "plan",
        "authorised_by": authorised_by if execute else "",
        "authority_evidence": authority_evidence if execute else "",
        "plan_sha256": plan_sha256,
        "eligible": [row["path"] for row in eligible_rows],
        "eligible_artifacts": eligible_rows,
        "removed": removed,
        "unknown_files_removed": False,
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("receipt", type=Path)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument("--workspace-root", type=Path, default=Path.cwd())
    parser.add_argument("--authorised-by", default="")
    parser.add_argument("--authority-evidence", default="")
    parser.add_argument("--approved-plan-sha256", default="")
    parser.add_argument("--product-root", type=Path, default=PRODUCT_ROOT)
    args = parser.parse_args(argv)
    try:
        result = cleanup(
            args.receipt,
            workspace_root=args.workspace_root,
            execute=args.execute,
            authorised_by=args.authorised_by,
            authority_evidence=args.authority_evidence,
            approved_plan_sha256=args.approved_plan_sha256,
            product_root=args.product_root.resolve(),
        )
    except CleanupError as exc:
        print(f"FAIL: {exc}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 1 if result["status"] == "fail" else 0


if __name__ == "__main__":
    raise SystemExit(main())
