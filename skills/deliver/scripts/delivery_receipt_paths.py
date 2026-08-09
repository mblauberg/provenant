"""Workspace containment and authority scoping for delivery receipt paths.

Every path a receipt records has to clear two separate checks: it must stay
inside the workspace, and it must fall within the authority scope declared for
its kind. Both are security boundaries, so they live in one module rather than
being restated at each call site.

The error type is injected the same way `delivery_receipt_process` takes it, so
this module stays free of a back-import into the producer.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

SCOPE_KINDS = {
    "artifact": ("allowed_artifact_paths", "artifact path"),
    "source": ("allowed_source_paths", "evidence source"),
}


def safe_workspace_path(
    workspace: Path, value: str, field: str, error_type: type[ValueError],
) -> tuple[Path, str]:
    path = Path(value)
    if path.is_absolute() or ".." in path.parts or not value:
        raise error_type(f"{field} must be safe and workspace-relative")
    target = (workspace / path).resolve()
    try:
        target.relative_to(workspace)
    except ValueError as exc:
        raise error_type(f"{field} escapes the workspace") from exc
    return target, path.as_posix().rstrip("/")


def ensure_within_scope(
    run: dict[str, Any],
    workspace: Path,
    target: Path,
    kind: str,
    error_type: type[ValueError],
) -> None:
    key, noun = SCOPE_KINDS[kind]
    target = target.resolve()
    authority = run.get("authority")
    scopes = authority.get(key) if isinstance(authority, dict) else None
    if not isinstance(scopes, list) or not scopes:
        raise error_type(f"authority.{key} must be a non-empty list")
    roots: list[Path] = []
    for scope in scopes:
        if not isinstance(scope, str):
            raise error_type(f"authority.{key} contains an invalid path")
        root, _relative = safe_workspace_path(
            workspace, scope, f"authority.{key}", error_type,
        )
        roots.append(root)
    if not any(target == root or target.is_relative_to(root) for root in roots):
        raise error_type(f"{noun} leaves authority.{key}")


def check_evidence_sources(
    run: dict[str, Any],
    workspace: Path,
    source_paths: list[str],
    error_type: type[ValueError],
    *,
    after_command: bool = False,
) -> None:
    suffix = " after command execution" if after_command else ""
    for source in source_paths:
        target, _relative = safe_workspace_path(
            workspace, source, "evidence source", error_type,
        )
        ensure_within_scope(run, workspace, target, "source", error_type)
        if not target.exists():
            raise error_type(f"evidence source does not exist{suffix}: {source}")
