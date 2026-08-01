from skills._shared.review_terminal import normalise_dispatch_review


def dispatch_result(**overrides):
    result = {
        "status": "ok", "exit": 0, "terminal_observed": True,
        "output_path": "reviews/luna.result.json",
        "adapter": "codex", "provider_family": "openai", "model_family": "openai",
        "endpoint_provider": "openai", "orchestrator_family": "anthropic",
        "read_only_guarantee": "enforced",
    }
    result.update(overrides)
    return result


def terminal_result(verdict="approve", **overrides):
    result = {
        "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "worker completed", "verdict": verdict,
    }
    result.update(overrides)
    return result


def test_review_verdict_must_come_from_the_worker_terminal_artifact():
    leg = normalise_dispatch_review(
        dispatch_result(), terminal_result(verdict="block"),
        review_verdict="approve", chair_family="anthropic",
        transcript_available=True, dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "verdict-mismatch"
    assert leg["certifying_vote"] is False


def test_same_endpoint_provider_does_not_certify_even_with_a_different_model_family():
    leg = normalise_dispatch_review(
        dispatch_result(model_family="anthropic", orchestrator_family="openai"),
        terminal_result(), review_verdict="approve", chair_family="openai",
        transcript_available=True, dispatcher_output_available=True,
    )

    assert leg["status"] == "pass"
    assert leg["reason"] == "same-provider-lineage"
    assert leg["certifying_vote"] is False


def test_unobserved_exit_is_not_a_review_terminal():
    leg = normalise_dispatch_review(
        dispatch_result(terminal_observed=False), terminal_result(),
        review_verdict="approve", chair_family="anthropic",
        transcript_available=True, dispatcher_output_available=True,
    )

    assert leg["status"] == "unavailable"
    assert leg["reason"] == "terminal-unavailable"
