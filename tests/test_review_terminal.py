import pytest

from skills._shared.review_terminal import normalise_dispatch_review


def dispatch_result(**overrides):
    result = {
        "status": "ok",
        "exit": 0,
        "output_path": "reviews/luna.out",
        "provider_family": "openai",
        "model_family": "openai",
        "certification_eligible": True,
    }
    result.update(overrides)
    return result


def test_completed_dispatch_review_is_a_certifying_ladder_leg():
    leg = normalise_dispatch_review(
        dispatch_result(),
        {"verdict": "approve"},
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg == {
        "family": "openai",
        "provider_family": "openai",
        "status": "pass",
        "reason": "",
        "terminal_available": True,
        "certifying_vote": True,
    }


@pytest.mark.parametrize(
    ("overrides", "review_result", "transcript_available", "reason"),
    [
        ({"exit": None}, {"verdict": "approve"}, True, "terminal-unavailable"),
        ({}, {}, True, "no-verdict"),
        ({}, {"verdict": "approve"}, False, "missing-transcript"),
        ({"status": "unavailable"}, {"verdict": "approve"}, True, "provider-unavailable"),
        ({"exit": 9}, {"verdict": "approve"}, True, "nonzero-exit"),
    ],
)
def test_incomplete_dispatch_review_is_explicit_and_non_certifying(
    overrides, review_result, transcript_available, reason
):
    leg = normalise_dispatch_review(
        dispatch_result(**overrides),
        review_result,
        transcript_available=transcript_available,
        dispatcher_output_available=True,
    )

    assert leg["status"] != "pass"
    assert leg["reason"] == reason
    assert leg["certifying_vote"] is False


def test_model_family_is_not_relabelled_as_dispatch_provider_family():
    leg = normalise_dispatch_review(
        dispatch_result(provider_family="cursor", model_family="xai"),
        {"verdict": "approve"},
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["family"] == "xai"
    assert leg["provider_family"] == "cursor"
    assert leg["status"] == "pass"


def test_missing_provider_family_fails_closed():
    leg = normalise_dispatch_review(
        dispatch_result(provider_family="", model_family=""),
        {"verdict": "approve"},
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "unavailable"
    assert leg["reason"] == "provider-unavailable"
    assert leg["certifying_vote"] is False


def test_invalid_verdict_is_not_certifying():
    leg = normalise_dispatch_review(
        dispatch_result(),
        {"verdict": "EXECUTION-UNAVAILABLE"},
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "no-verdict"
    assert leg["certifying_vote"] is False


def test_missing_dispatcher_output_is_not_a_transcript():
    leg = normalise_dispatch_review(
        dispatch_result(output_path="reviews/missing.out"),
        {"verdict": "approve"},
        transcript_available=True,
        dispatcher_output_available=False,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "missing-transcript"
    assert leg["certifying_vote"] is False


def test_missing_provider_family_fails_closed_even_with_model_family():
    leg = normalise_dispatch_review(
        dispatch_result(provider_family="", model_family="xai"),
        {"verdict": "approve"},
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "unavailable"
    assert leg["reason"] == "provider-unavailable"
    assert leg["certifying_vote"] is False
