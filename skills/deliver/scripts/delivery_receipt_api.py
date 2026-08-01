"""Narrow lifecycle adapter exposed by the receipt producer."""

from __future__ import annotations

from typing import Any


def build(api: Any) -> dict[str, Any]:
    return {
        "ReceiptError": api.ReceiptError,
        "PROFILE_PATH": api.PROFILE_PATH,
        "CHAIR_FAMILIES": api.CHAIR_FAMILIES,
        "TRANSITIONS": api._transitions(),
        "SIDE_STATES": api._side_states(),
        "mutate_receipt": api.mutate_receipt,
        "mutate_receipt_with_artifacts": api.mutate_receipt_with_artifacts,
        "safe_workspace_path": api.safe_workspace_path,
        "ensure_allowed_source_target": api.ensure_allowed_source_target,
        "ensure_allowed_artifact_target": api.ensure_allowed_artifact_target,
        "require_identifier": api.require_identifier,
        "find_artifact": api.find_artifact,
        "ensure_new_evidence_id": api.ensure_new_evidence_id,
        "digest_bytes": api.digest_bytes,
        "timestamp_after": api.timestamp_after,
        "parse_utc": api.parse_utc,
        "utc_now": api.utc_now,
        "run_lock": api.run_lock,
        "receipt_path": api._receipt_path,
        "resolve_run_dir": api.resolve_run_dir,
        "load_run": api.load_run,
        "ensure_immutable_risk": api.ensure_immutable_risk,
        "ensure_mutable": api.ensure_mutable,
        "write_json_atomic": api.write_json_atomic,
        "create_json_exclusive": api.create_json_exclusive,
        "bundle_artifact": api._bundle_artifact,
        "bundle_bytes": api._bundle_bytes,
        "hashed_bundle_path": api._hashed_bundle_path,
        "publish_bundle_and_receipt": api._publish_bundle_and_receipt,
    }
