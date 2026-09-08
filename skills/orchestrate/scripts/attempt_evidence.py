"""Private shared validation for a retained successful dispatch attempt.

Execution publishes attempt evidence; batch collection, recovery, controls and
finalisation consume it.  This module owns only the success meaning shared by
those consumers.  It neither starts processes nor writes run records.
"""

from __future__ import annotations

import hashlib
import json
from pathlib import Path
from typing import Any


class AttemptEvidenceError(ValueError):
    """A retained successful attempt cannot support a success claim."""


def digest_bytes(value: bytes) -> str:
    return f"sha256:{hashlib.sha256(value).hexdigest()}"


def successful_adapter_error(
    adapter: dict[str, Any], expected_adapter: object, expected_intent: object,
    result_path: Path, result_digest: str,
) -> str | None:
    """Return the stable reason when an adapter cannot substantiate success."""
    required = (
        "tool", "adapter", "execution_intent", "resolved_model", "provider_family",
        "model_family", "endpoint_provider", "identity_source", "output_path", "output_digest",
        "read_only_guarantee",
    )
    if any(not isinstance(adapter.get(field), str) or not adapter[field] for field in required):
        return "successful adapter receipt is missing route identity"
    if adapter["tool"] != expected_adapter or adapter["adapter"] != expected_adapter:
        return "successful adapter receipt does not match the requested adapter"
    if adapter["execution_intent"] != expected_intent:
        return "successful adapter receipt does not match the requested intent"
    try:
        output_matches = Path(adapter["output_path"]).resolve() == result_path.resolve()
    except (OSError, TypeError, ValueError):
        output_matches = False
    if not output_matches or adapter["output_digest"] != result_digest:
        return "successful adapter receipt does not match the retained output"
    if not isinstance(adapter.get("exit"), int) or isinstance(adapter["exit"], bool) or adapter["exit"] != 0:
        return "successful adapter receipt must record exit 0"
    if not isinstance(adapter.get("cross_family"), bool) or not isinstance(adapter.get("certification_eligible"), bool):
        return "successful adapter receipt is missing assurance flags"
    if expected_intent == "ordinary" and adapter["certification_eligible"]:
        return "ordinary execution cannot be certification eligible"
    return None


def validate_successful_attempt(
    run_dir: Path, record: dict[str, Any], payloads: dict[str, bytes]
) -> None:
    """Validate terminal process, retained result, and adapter agreement once."""
    if record.get("status") != "succeeded":
        raise AttemptEvidenceError("successful attempt status is invalid")
    process = record.get("process")
    if not isinstance(process, dict) or process.get("observed_exit") is not True:
        raise AttemptEvidenceError("successful attempt does not prove process exit")
    exit_code = process.get("exit_code")
    if not isinstance(exit_code, int) or isinstance(exit_code, bool) or exit_code != 0:
        raise AttemptEvidenceError("successful attempt does not prove exit 0")
    result = record.get("result")
    result_bytes = payloads.get("result")
    if not isinstance(result, dict) or result_bytes is None:
        raise AttemptEvidenceError("successful attempt has no result evidence")
    if not result_bytes:
        raise AttemptEvidenceError("successful attempt result is empty")
    result_path = result.get("path")
    if not isinstance(result_path, str) or result.get("digest") != digest_bytes(result_bytes):
        raise AttemptEvidenceError("successful attempt result digest is invalid")
    adapter_bytes = payloads.get("adapter_receipt")
    if adapter_bytes is None:
        raise AttemptEvidenceError("successful attempt has no adapter receipt")
    try:
        lines = [line for line in adapter_bytes.decode("utf-8").splitlines() if line.strip()]
        adapter = json.loads(lines[-1]) if lines else None
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise AttemptEvidenceError("successful adapter receipt is not valid JSON") from exc
    if not isinstance(adapter, dict) or adapter.get("status") != "ok":
        raise AttemptEvidenceError("successful adapter receipt is invalid")
    requested = record.get("requested_route")
    route = record.get("route")
    if not isinstance(requested, dict) or not isinstance(route, dict):
        raise AttemptEvidenceError("successful attempt route is invalid")
    reason = successful_adapter_error(
        adapter, requested.get("adapter"), requested.get("intent"),
        run_dir / result_path, digest_bytes(result_bytes),
    )
    if reason is not None:
        raise AttemptEvidenceError(reason)
    for field in ("adapter", "execution_intent", "provider_family", "resolved_model"):
        if route.get(field) != adapter.get(field):
            raise AttemptEvidenceError(f"successful attempt route disagrees with adapter receipt: {field}")
