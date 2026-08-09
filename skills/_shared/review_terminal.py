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
_EXECUTION_UNAVAILABLE = "EXECUTION-UNAVAILABLE"
REVIEW_RESULT_KEYS = frozenset({"angle", "verdict", "issues", "crossFamily", "path"})
_REVIEW_ISSUE_KEYS = frozenset({"severity", "patchPath", "detail"})
_REVIEW_CROSS_FAMILY_KEYS = frozenset({
    "ran", "tool", "status", "modelFamily", "endpointProvider", "crossFamily",
    "certificationEligible", "readOnlyGuarantee", "outputPath", "routeReceipt", "notRunReason",
})
_ENDPOINT_PROVIDERS = {
    "claude": "anthropic",
    "codex": "openai",
    "cursor": "cursor",
    "kiro": "aws",
    "copilot": "github",
}
_FIXED_PROVIDER_FAMILIES = {"claude": "anthropic", "codex": "openai"}
_FIXED_MODEL_FAMILIES = {"claude": "anthropic", "codex": "openai"}


def _family(result: Mapping[str, Any]) -> tuple[str, str]:
    """Keep model lineage and dispatch endpoint family separately."""

    model_family = result.get("model_family")
    provider_family = result.get("provider_family")
    return (
        model_family.strip() if isinstance(model_family, str) else "",
        provider_family.strip() if isinstance(provider_family, str) else "",
    )


def _terminal_verdict(terminal_result: object) -> tuple[str, str]:
    if not isinstance(terminal_result, Mapping):
        return "", "no-verdict"
    if "verdict" not in terminal_result:
        return "", "no-verdict"
    if not _is_review_result(terminal_result):
        return "", "invalid-terminal-result"
    verdict = terminal_result.get("verdict")
    if not isinstance(verdict, str):
        return "", "no-verdict"
    verdict = verdict.strip()
    if verdict in _REVIEW_VERDICTS:
        return verdict, ""
    if verdict == _EXECUTION_UNAVAILABLE:
        return "", "execution-unavailable"
    return "", "unparseable-verdict"


def _is_review_result(value: Mapping[str, Any]) -> bool:
    """Validate the worker result shape shared by the review workflow."""

    if set(value) != REVIEW_RESULT_KEYS:
        return False
    if not isinstance(value.get("angle"), str) or not isinstance(value.get("path"), str):
        return False
    issues = value.get("issues")
    if not isinstance(issues, list):
        return False
    for issue in issues:
        if (
            not isinstance(issue, Mapping)
            or set(issue) != _REVIEW_ISSUE_KEYS
            or issue.get("severity") not in {"P0", "P1", "P2"}
            or not all(isinstance(issue.get(field), str) for field in ("patchPath", "detail"))
        ):
            return False
    cross_family = value.get("crossFamily")
    if not isinstance(cross_family, Mapping) or set(cross_family) != _REVIEW_CROSS_FAMILY_KEYS:
        return False
    if not all(isinstance(cross_family.get(field), bool) for field in ("ran", "crossFamily", "certificationEligible")):
        return False
    return all(
        isinstance(cross_family.get(field), str)
        for field in (
            "tool", "status", "modelFamily", "endpointProvider", "readOnlyGuarantee",
            "outputPath", "routeReceipt", "notRunReason",
        )
    )


def _lineage(result: Mapping[str, Any], chair_family: object) -> tuple[str, str, str, str, bool, str]:
    family, provider_family = _family(result)
    adapter = result.get("adapter", result.get("tool"))
    tool = result.get("tool")
    endpoint_provider = result.get("endpoint_provider")
    orchestrator_family = result.get("orchestrator_family")
    endpoint_provider = endpoint_provider.strip() if isinstance(endpoint_provider, str) else ""
    orchestrator_family = orchestrator_family.strip() if isinstance(orchestrator_family, str) else ""
    chair = chair_family.strip() if isinstance(chair_family, str) else ""
    if not family or not provider_family or not endpoint_provider or not orchestrator_family:
        return family, provider_family, endpoint_provider, orchestrator_family, False, "provider-unavailable"
    if not isinstance(adapter, str) or not adapter:
        return family, provider_family, endpoint_provider, orchestrator_family, False, "endpoint/provider identity unavailable"
    if not isinstance(tool, str) or not tool or tool != adapter:
        return family, provider_family, endpoint_provider, orchestrator_family, False, "tool/adapter identity mismatch"
    expected_endpoint = _ENDPOINT_PROVIDERS.get(adapter)
    if expected_endpoint is None or endpoint_provider != expected_endpoint:
        return family, provider_family, endpoint_provider, orchestrator_family, False, "endpoint/provider identity mismatch"
    expected_provider = _FIXED_PROVIDER_FAMILIES.get(adapter)
    if expected_provider is not None and provider_family != expected_provider:
        return family, provider_family, endpoint_provider, orchestrator_family, False, "endpoint/provider identity mismatch"
    expected_model_family = _FIXED_MODEL_FAMILIES.get(adapter)
    if expected_model_family is not None and family != expected_model_family:
        return family, provider_family, endpoint_provider, orchestrator_family, False, "endpoint/provider/model identity mismatch"
    if not chair:
        return family, provider_family, endpoint_provider, orchestrator_family, False, "chair-family-unavailable"
    if orchestrator_family != chair:
        return family, provider_family, endpoint_provider, orchestrator_family, False, "orchestrator-family-mismatch"
    if provider_family == chair:
        return family, provider_family, endpoint_provider, orchestrator_family, False, "same-provider-lineage"
    return family, provider_family, endpoint_provider, orchestrator_family, True, ""


def normalise_dispatch_review(
    dispatch_result: Mapping[str, Any],
    terminal_result: object,
    *,
    review_verdict: object,
    chair_family: object,
    transcript_available: bool,
    dispatcher_output_available: bool,
) -> dict[str, object]:
    """Return the small leg shape consumed by ``check_review_ladder``.

    ``exit`` is the dispatcher's observed process exit.  Its absence is not
    treated as a successful legacy result.  ``transcript_available`` is kept
    as an adapter argument because the dispatcher result identifies the output
    path but does not own review-artifact custody.
    """

    family, provider_family, endpoint_provider, orchestrator_family, cross_family, lineage_reason = _lineage(
        dispatch_result, chair_family
    )
    exit_value = dispatch_result.get("exit")
    terminal_available = isinstance(exit_value, int) and not isinstance(exit_value, bool)
    status = dispatch_result.get("status")
    output_path = dispatch_result.get("output_path")

    worker_verdict, worker_reason = _terminal_verdict(terminal_result)
    wrapper_verdict = review_verdict.strip() if isinstance(review_verdict, str) else ""
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
    elif not worker_verdict:
        leg_status = "unavailable" if worker_reason == "execution-unavailable" else "failed"
        reason = worker_reason
    elif wrapper_verdict not in _REVIEW_VERDICTS:
        leg_status = "failed"
        reason = "wrapper-verdict-unavailable"
    elif worker_verdict != wrapper_verdict:
        leg_status = "failed"
        reason = "verdict-mismatch"
    elif lineage_reason == "provider-unavailable":
        leg_status = "unavailable"
        reason = lineage_reason
    elif lineage_reason.startswith(("endpoint/provider", "tool/adapter")):
        leg_status = "failed"
        reason = lineage_reason
    elif lineage_reason == "orchestrator-family-mismatch":
        leg_status = "failed"
        reason = lineage_reason
    elif status != "ok":
        leg_status = "failed"
        reason = "dispatch-failed"
    elif not cross_family:
        reason = lineage_reason
    elif dispatch_result.get("read_only_guarantee") not in {"enforced", "oauth_safe_mode"}:
        reason = "read-only-guarantee-unavailable"

    return {
        "family": family,
        "provider_family": provider_family,
        "endpoint_provider": endpoint_provider,
        "orchestrator_family": orchestrator_family,
        "cross_family": cross_family,
        "status": leg_status,
        "reason": reason,
        "terminal_available": terminal_available,
        "certifying_vote": (
            leg_status == "pass"
            and cross_family
            and dispatch_result.get("read_only_guarantee") in {"enforced", "oauth_safe_mode"}
        ),
    }
