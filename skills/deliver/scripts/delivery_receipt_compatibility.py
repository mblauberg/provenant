"""Validation for the deliberately narrow pre-lifecycle checkpoint shape."""

from __future__ import annotations

from typing import Any, Callable


def validate_checkpoint(
    value: Any, error_type: type[ValueError], canonical_validator: Callable[[Any], None],
) -> None:
    if not isinstance(value, dict):
        raise error_type("RUN.json root must be an object")
    if value.get("schema_version") != 1 or value.get("contract") != "delivery-run":
        raise error_type("RUN.json must be a canonical delivery-run v1 receipt")
    if "run_id" in value:
        canonical_validator(value)
        return
    authority = value.get("authority")
    scopes = authority.get("allowed_artifact_paths") if isinstance(authority, dict) else None
    if not isinstance(scopes, list) or not scopes or any(not isinstance(scope, str) or not scope for scope in scopes):
        raise error_type("authority.allowed_artifact_paths must be a non-empty list")
    checkpoint = value.get("checkpoint")
    if not isinstance(checkpoint, dict):
        raise error_type("RUN.json checkpoint must be an object")
    generation = checkpoint.get("generation")
    if isinstance(generation, bool) or not isinstance(generation, int) or generation < 0:
        raise error_type("checkpoint.generation must be a non-negative integer")
    for field in ("current_slice", "next_action"):
        if not isinstance(checkpoint.get(field), str) or not checkpoint[field].strip():
            raise error_type(f"checkpoint.{field} must be a non-empty string")
    for field in ("in_flight", "artifact_paths"):
        rows = checkpoint.get(field)
        if not isinstance(rows, list) or any(not isinstance(item, str) or not item for item in rows):
            raise error_type(f"checkpoint.{field} must be a list of non-empty strings")
    if value.get("status") == "closed":
        raise error_type("closed run is immutable")
