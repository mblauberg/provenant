import pytest

from skills._shared.review_terminal import normalise_dispatch_review


def dispatch_result(**overrides):
    result = {
        "status": "ok",
        "exit": 0,
        "output_path": "reviews/luna.out",
        "tool": "codex",
        "adapter": "codex",
        "provider_family": "openai",
        "model_family": "openai",
        "endpoint_provider": "openai",
        "orchestrator_family": "anthropic",
        "read_only_guarantee": "enforced",
        "certification_eligible": True,
        "cross_family": True,
    }
    result.update(overrides)
    return result


def worker_result(verdict="approve", **overrides):
    result = {
        "angle": "correctness",
        "verdict": verdict,
        "issues": [],
        "crossFamily": {
            "ran": True,
            "tool": "codex",
            "status": "ok",
            "modelFamily": "openai",
            "endpointProvider": "openai",
            "crossFamily": True,
            "certificationEligible": True,
            "readOnlyGuarantee": "enforced",
            "outputPath": "reviews/luna.out",
            "routeReceipt": "reviews/luna.route.json",
            "notRunReason": "",
        },
        "path": "reviews/luna.md",
    }
    result.update(overrides)
    return result


def test_completed_dispatch_review_is_a_certifying_ladder_leg():
    leg = normalise_dispatch_review(
        dispatch_result(),
        worker_result(),
        review_verdict="approve",
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg == {
        "family": "openai",
        "provider_family": "openai",
        "status": "pass",
        "reason": "",
        "terminal_available": True,
        "endpoint_provider": "openai",
        "orchestrator_family": "anthropic",
        "cross_family": True,
        "certifying_vote": True,
    }


@pytest.mark.parametrize(
    ("overrides", "review_result", "transcript_available", "reason"),
    [
        ({"exit": None}, worker_result(), True, "terminal-unavailable"),
        ({}, {}, True, "no-verdict"),
        ({}, worker_result(), False, "missing-transcript"),
        ({"status": "unavailable"}, worker_result(), True, "provider-unavailable"),
        ({"exit": 9}, worker_result(), True, "nonzero-exit"),
    ],
)
def test_incomplete_dispatch_review_is_explicit_and_non_certifying(
    overrides, review_result, transcript_available, reason
):
    leg = normalise_dispatch_review(
        dispatch_result(**overrides),
        review_result,
        review_verdict="approve" if review_result else "",
        chair_family="anthropic",
        transcript_available=transcript_available,
        dispatcher_output_available=True,
    )

    assert leg["status"] != "pass"
    assert leg["reason"] == reason
    assert leg["certifying_vote"] is False


def test_model_family_is_not_relabelled_as_dispatch_provider_family():
    leg = normalise_dispatch_review(
        dispatch_result(tool="cursor", provider_family="cursor", model_family="xai", adapter="cursor", endpoint_provider="cursor"),
        worker_result(),
        review_verdict="approve",
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["family"] == "xai"
    assert leg["provider_family"] == "cursor"
    assert leg["status"] == "pass"


def test_clean_distinct_provider_lineage_is_certifying_even_without_route_booleans():
    leg = normalise_dispatch_review(
        dispatch_result(
            tool="cursor",
            adapter="cursor",
            provider_family="xai",
            model_family="xai",
            endpoint_provider="cursor",
            orchestrator_family="openai",
            cross_family=False,
            certification_eligible=False,
        ),
        worker_result(),
        review_verdict="approve",
        chair_family="openai",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "pass"
    assert leg["provider_family"] == "xai"
    assert leg["endpoint_provider"] == "cursor"
    assert leg["certifying_vote"] is True


def test_missing_provider_family_fails_closed():
    leg = normalise_dispatch_review(
        dispatch_result(provider_family="", model_family=""),
        worker_result(),
        review_verdict="approve",
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "unavailable"
    assert leg["reason"] == "provider-unavailable"
    assert leg["certifying_vote"] is False


def test_invalid_verdict_is_not_certifying():
    leg = normalise_dispatch_review(
        dispatch_result(),
        worker_result("EXECUTION-UNAVAILABLE"),
        review_verdict="approve",
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "unavailable"
    assert leg["reason"] == "execution-unavailable"
    assert leg["certifying_vote"] is False


def test_unparseable_worker_verdict_is_explicitly_non_certifying():
    leg = normalise_dispatch_review(
        dispatch_result(),
        worker_result("not-a-review-verdict"),
        review_verdict="approve",
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "unparseable-verdict"
    assert leg["certifying_vote"] is False


def test_missing_dispatcher_output_is_not_a_transcript():
    leg = normalise_dispatch_review(
        dispatch_result(output_path="reviews/missing.out"),
        worker_result(),
        review_verdict="approve",
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=False,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "missing-transcript"
    assert leg["certifying_vote"] is False


def test_missing_provider_family_fails_closed_even_with_model_family():
    leg = normalise_dispatch_review(
        dispatch_result(provider_family="", model_family="xai"),
        worker_result(),
        review_verdict="approve",
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "unavailable"
    assert leg["reason"] == "provider-unavailable"
    assert leg["certifying_vote"] is False


def test_same_provider_cannot_certify_a_different_model_family():
    leg = normalise_dispatch_review(
        dispatch_result(provider_family="openai", model_family="anthropic", orchestrator_family="openai"),
        worker_result(),
        review_verdict="approve",
        chair_family="openai",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["provider_family"] == "openai"
    assert leg["family"] == "anthropic"
    assert leg["certifying_vote"] is False
    assert leg["reason"] == "endpoint/provider/model identity mismatch"


def test_adapter_model_family_must_match_its_route_identity():
    leg = normalise_dispatch_review(
        dispatch_result(model_family="anthropic"),
        worker_result(),
        review_verdict="approve",
        chair_family="openai",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "endpoint/provider/model identity mismatch"
    assert leg["certifying_vote"] is False


def test_tool_and_adapter_must_be_the_same_dispatch_identity():
    leg = normalise_dispatch_review(
        dispatch_result(tool="codex", adapter="cursor", endpoint_provider="cursor", provider_family="xai", model_family="xai"),
        worker_result(),
        review_verdict="approve",
        chair_family="openai",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "tool/adapter identity mismatch"
    assert leg["certifying_vote"] is False


def test_missing_worker_verdict_is_noncertifying_even_with_wrapper_verdict():
    leg = normalise_dispatch_review(
        dispatch_result(),
        {"summary": "provider stopped without a verdict"},
        review_verdict="approve",
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "no-verdict"
    assert leg["certifying_vote"] is False
