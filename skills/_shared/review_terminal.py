"""Normalize an existing dispatcher result at the review-ladder boundary.

The dispatcher owns process execution and emits its established result record.
This adapter only decides whether that record carries enough terminal evidence
for a review leg to be represented as a passing ladder leg.  The worker
terminal contract remains owned by the worker-contract lane.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any


_UNAVAILABLE_STATUSES = frozenset({
    "adapter_account_default_only",
    "all_failed",
    "auth_or_quota_error",
    "model_required_for_broker",
    "provider_unavailable",
    "routing_record_invalid",
    "same_family_forbidden",
    "tool_not_found",
    "unknown_tool",
    "unsafe_by_default",
    "unavailable",
})
_REVIEW_VERDICTS = frozenset({"approve", "approve-with-nits", "block"})


def _family(result: Mapping[str, Any]) -> tuple[str, str]:
    """Keep model lineage and dispatch endpoint family separately."""

    model_family = result.get("model_family")
    provider_family = result.get("provider_family")
    return (
        model_family.strip() if isinstance(model_family, str) else "",
        provider_family.strip() if isinstance(provider_family, str) else "",
    )


def _has_verdict(review_result: object) -> bool:
    if not isinstance(review_result, Mapping):
        return False
    verdict = review_result.get("verdict")
    return isinstance(verdict, str) and verdict.strip() in _REVIEW_VERDICTS


def normalise_dispatch_review(
    dispatch_result: Mapping[str, Any],
    review_result: object,
    *,
    transcript_available: bool,
    dispatcher_output_available: bool,
) -> dict[str, object]:
    """Return the small leg shape consumed by ``check_review_ladder``.

    ``exit`` is the dispatcher's observed process exit.  Its absence is not
    treated as a successful legacy result.  ``transcript_available`` is kept
    as an adapter argument because the dispatcher result identifies the output
    path but does not own review-artifact custody.
    """

    family, provider_family = _family(dispatch_result)
    exit_value = dispatch_result.get("exit")
    terminal_available = isinstance(exit_value, int) and not isinstance(exit_value, bool)
    status = dispatch_result.get("status")
    output_path = dispatch_result.get("output_path")

    reason = ""
    leg_status = "pass"
    if not terminal_available:
        leg_status = "unavailable"
        reason = "terminal-unavailable"
    elif status in _UNAVAILABLE_STATUSES:
        leg_status = "unavailable"
        reason = "provider-unavailable"
    elif exit_value != 0:
        leg_status = "failed"
        reason = "nonzero-exit"
    elif (
        not isinstance(output_path, str)
        or not output_path.strip()
        or not dispatcher_output_available
        or not transcript_available
    ):
        leg_status = "failed"
        reason = "missing-transcript"
    elif not _has_verdict(review_result):
        leg_status = "failed"
        reason = "no-verdict"
    elif not family or not provider_family:
        leg_status = "unavailable"
        reason = "provider-unavailable"
    elif status != "ok":
        leg_status = "failed"
        reason = "dispatch-failed"

    return {
        "family": family,
        "provider_family": provider_family,
        "status": leg_status,
        "reason": reason,
        "terminal_available": terminal_available,
        "certifying_vote": leg_status == "pass" and dispatch_result.get("certification_eligible") is True,
    }
