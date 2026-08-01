import hashlib
import importlib.util
import json
from pathlib import Path
import sys


ROOT = Path(__file__).resolve().parents[1]
MODULE = ROOT / "skills" / "_shared" / "worker_outcome.py"
SPEC = importlib.util.spec_from_file_location("worker_outcome", MODULE)
worker_outcome = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
sys.modules[SPEC.name] = worker_outcome
SPEC.loader.exec_module(worker_outcome)


def _digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def _reference(run: Path, *, terminal: dict, dispatch: dict | None = None):
    terminal_path = run / "worker.json"
    if terminal.get("kind") == "complete" and "verdict" not in terminal:
        terminal["verdict"] = "pass"
    terminal_path.write_text(json.dumps(terminal), encoding="utf-8")
    answer_path = run / "answer.txt"
    answer_path.write_text("human answer\n", encoding="utf-8")
    dispatch_terminal_path = run / "dispatch-terminal.json"
    dispatch_terminal_path.write_text(json.dumps({
        "id": terminal["id"], "attempt_id": terminal["attempt_id"],
        "kind": "complete", "summary": "dispatcher observed terminality",
    }), encoding="utf-8")
    dispatch_path = run / "dispatch.json"
    dispatch_path.write_text(json.dumps(dispatch or {
        "id": terminal["id"],
        "attempt_id": terminal["attempt_id"],
        "status": "ok",
        "exit": 0,
        "terminal_observed": True,
        "output_path": "answer.txt",
        "output_sha256": _digest(answer_path),
        "terminal_artifact_path": "dispatch-terminal.json",
        "terminal_artifact_sha256": _digest(dispatch_terminal_path),
    }), encoding="utf-8")
    return {
        "id": terminal["id"],
        "dispatch_receipt": {"path": "dispatch.json", "digest": _digest(dispatch_path)},
        "terminal_artifact": {"path": "worker.json", "digest": _digest(terminal_path)},
        "dispatch_terminal_artifact": {
            "path": "dispatch-terminal.json", "digest": _digest(dispatch_terminal_path),
        },
        "worktree_receipt": None,
    }


def test_accept_worker_outcome_joins_observed_complete_artifact(tmp_path):
    reference = _reference(tmp_path, terminal={
        "id": "review-1",
        "attempt_id": "attempt-1",
        "kind": "complete",
        "summary": "review completed",
        "verdict": "pass",
    })

    result = worker_outcome.accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "accepted"
    assert result["kind"] == "complete"
    assert result["verdict"] == "pass"


def test_live_or_unobserved_dispatch_cannot_certify_terminal_artifact(tmp_path):
    reference = _reference(tmp_path, terminal={
        "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "review completed", "verdict": "pass",
    }, dispatch={
        "id": "review-1", "attempt_id": "attempt-1", "status": "ok", "exit": 0,
        "terminal_observed": False, "output_path": "worker.json",
        "output_sha256": "placeholder",
    })

    result = worker_outcome.accept_worker_outcome(tmp_path, reference)

    assert result == {"status": "rejected", "reason": "dispatch receipt does not prove observed terminality"}


def test_terminal_digest_and_output_digest_must_bind_the_same_bytes(tmp_path):
    reference = _reference(tmp_path, terminal={
        "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "review completed", "verdict": "pass",
    })
    dispatch = json.loads((tmp_path / "dispatch.json").read_text())
    dispatch["output_sha256"] = "sha256:" + "0" * 64
    dispatch_path = tmp_path / "dispatch.json"
    dispatch_path.write_text(json.dumps(dispatch))
    reference["dispatch_receipt"]["digest"] = _digest(dispatch_path)

    result = worker_outcome.accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert "answer digest" in result["reason"]


def test_separate_answer_and_terminal_artifact_are_verified_independently(tmp_path):
    answer_path = tmp_path / "answer.txt"
    answer_path.write_text("human answer\n", encoding="utf-8")
    terminal_path = tmp_path / "worker.json"
    terminal_path.write_text(json.dumps({
        "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "terminal result", "verdict": "pass",
    }), encoding="utf-8")
    dispatch_terminal_path = tmp_path / "dispatch-terminal.json"
    dispatch_terminal_path.write_text(json.dumps({
        "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "dispatcher observed terminality",
    }), encoding="utf-8")
    dispatch_path = tmp_path / "dispatch.json"
    dispatch_path.write_text(json.dumps({
        "id": "review-1", "attempt_id": "attempt-1", "status": "ok", "exit": 0,
        "terminal_observed": True, "output_path": str(answer_path),
        "output_sha256": _digest(answer_path),
        "terminal_artifact_path": str(dispatch_terminal_path),
        "terminal_artifact_sha256": _digest(dispatch_terminal_path),
    }), encoding="utf-8")
    reference = {
        "id": "review-1",
        "dispatch_receipt": {"path": "dispatch.json", "digest": _digest(dispatch_path)},
        "terminal_artifact": {"path": "worker.json", "digest": _digest(terminal_path)},
        "dispatch_terminal_artifact": {
            "path": "dispatch-terminal.json", "digest": _digest(dispatch_terminal_path),
        },
        "worktree_receipt": None,
    }

    assert worker_outcome.accept_worker_outcome(tmp_path, reference)["status"] == "accepted"

    answer_path.write_text("tampered answer\n", encoding="utf-8")
    rejected = worker_outcome.accept_worker_outcome(tmp_path, reference)
    assert rejected["status"] == "rejected"
    assert "answer digest" in rejected["reason"]


def test_copied_identity_or_wrapper_fields_cannot_replace_worker_identity(tmp_path):
    reference = _reference(tmp_path, terminal={
        "id": "different-review", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "review completed", "verdict": "pass",
    })
    reference["id"] = "review-1"

    result = worker_outcome.accept_worker_outcome(tmp_path, reference)

    assert result == {"status": "rejected", "reason": "dispatch receipt identity does not match worker outcome"}


def test_question_is_an_accepted_noncertifying_terminal_outcome(tmp_path):
    reference = _reference(tmp_path, terminal={
        "id": "review-1", "attempt_id": "attempt-1", "kind": "question",
        "question": "Which authority should apply?",
    })

    result = worker_outcome.accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "accepted"
    assert result["kind"] == "question"
    assert result["certifying"] is False
    assert result["question"] == "Which authority should apply?"


def test_complete_outcome_requires_its_own_summary_and_rejects_reason_fields(tmp_path):
    reference = _reference(tmp_path, terminal={
        "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
        "reason": "wrapper says complete", "verdict": "pass",
    })

    result = worker_outcome.accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert "summary" in result["reason"] or "another outcome kind" in result["reason"]


def test_path_traversal_and_open_artifact_refs_fail_closed(tmp_path):
    reference = _reference(tmp_path, terminal={
        "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "review completed", "verdict": "pass", "artifact_refs": [{
            "path": "../outside", "digest": "sha256:" + "0" * 64,
        }],
    })

    result = worker_outcome.accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert "artifact_refs" in result["reason"]


def test_nonzero_dispatch_exit_is_explicit_and_never_certifying(tmp_path):
    reference = _reference(tmp_path, terminal={
        "id": "review-1", "attempt_id": "attempt-1", "kind": "failed",
        "reason": "provider exited with an error",
    })
    dispatch_path = tmp_path / "dispatch.json"
    dispatch = json.loads(dispatch_path.read_text())
    dispatch["status"] = "error"
    dispatch["exit"] = 9
    dispatch_path.write_text(json.dumps(dispatch))
    reference["dispatch_receipt"]["digest"] = _digest(dispatch_path)

    result = worker_outcome.accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "accepted"
    assert result["kind"] == "failed"
    assert result["certifying"] is False
    assert result["reason"] == "nonzero dispatch exit"


def test_claimed_change_requires_a_digest_bound_clean_base_descended_receipt(tmp_path):
    reference = _reference(tmp_path, terminal={
        "id": "implementation-1", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "implementation completed",
    })
    worktree_path = tmp_path / "worktree.json"
    worktree_path.write_text(json.dumps({
        "status": "accepted", "base_revision": "a" * 40,
        "head_revision": "b" * 40, "claimed_commit": "b" * 40, "clean": True,
    }))
    reference["worktree_receipt"] = {"path": "worktree.json", "digest": _digest(worktree_path)}

    result = worker_outcome.accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "accepted"
    assert result["certifying"] is True

    worktree = json.loads(worktree_path.read_text())
    worktree["clean"] = False
    worktree_path.write_text(json.dumps(worktree))
    reference["worktree_receipt"]["digest"] = _digest(worktree_path)
    rejected = worker_outcome.accept_worker_outcome(tmp_path, reference)
    assert rejected["status"] == "rejected"
    assert "clean worktree" in rejected["reason"]
