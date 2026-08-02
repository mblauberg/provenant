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


def wrapper_verdict(verdict="approve", **overrides):
    result = {
        "verdict": verdict,
        "model": "gpt-reviewer",
        "family": "openai",
        "endpoint": "openai",
        "route_receipt": "reviews/luna.route.json",
        "terminal_result": "reviews/luna.result.json",
    }
    result.update(overrides)
    return result


def test_worker_block_verdict_cannot_certify():
    leg = normalise_dispatch_review(
        dispatch_result(), terminal_result(verdict="block"), review_verdict="block",
        chair_family="anthropic",
        transcript_available=True, dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "worker verdict is block"
    assert leg["certifying_vote"] is False


def test_review_verdict_must_use_the_known_vocabulary():
    leg = normalise_dispatch_review(
        dispatch_result(), terminal_result(verdict="pass"), review_verdict="banana",
        chair_family="anthropic",
        transcript_available=True, dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "wrapper-verdict-unavailable"
    assert leg["certifying_vote"] is False


def test_review_verdict_must_come_from_the_worker_terminal_artifact():
    leg = normalise_dispatch_review(
        dispatch_result(), terminal_result(verdict="approve-with-nits"),
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
        dispatch_result(terminal_observed=False), terminal_result(), review_verdict="approve",
        chair_family="anthropic",
        transcript_available=True, dispatcher_output_available=True,
    )

    assert leg["status"] == "unavailable"
    assert leg["reason"] == "terminal-unavailable"


def test_verdict_disagreement_rejection():
    leg = normalise_dispatch_review(
        dispatch_result(),
        terminal_result(verdict="approve"),
        review_verdict=wrapper_verdict(verdict="block"),
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["status"] == "failed"
    assert leg["reason"] == "verdict-mismatch"
    assert leg["certifying_vote"] is False


def test_agent_lineage_not_trusted():
    agent_result = terminal_result(
        model="anthropic-agent",
        family="anthropic",
        endpoint="anthropic",
        route_receipt="agent-supplied.route.json",
        terminal_result="agent-supplied.result.json",
    )
    leg = normalise_dispatch_review(
        dispatch_result(),
        agent_result,
        wrapper_verdict=wrapper_verdict(),
        chair_family="anthropic",
        transcript_available=True,
        dispatcher_output_available=True,
    )

    assert leg["family"] == "openai"
    assert leg["provider_family"] == "openai"
    assert leg["endpoint_provider"] == "openai"
    assert leg["orchestrator_family"] == "anthropic"
