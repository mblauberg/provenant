import hashlib
import json
from pathlib import Path
import subprocess
import threading
from types import SimpleNamespace

import pytest

from skills.implement.scripts import accept_claim
import skills._shared.worker_outcome as worker_outcome
from skills._shared.worker_outcome import accept_worker_outcome
from skills.orchestrate.scripts import run_dir_finalize


def digest(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def write_attempt(
    run: Path,
    *,
    kind="complete",
    verdict="pass",
    status="ok",
    exit_code=0,
    directory: str | None = None,
):
    evidence = run / directory if directory else run
    evidence.mkdir(parents=True, exist_ok=True)
    prefix = f"{directory}/" if directory else ""
    answer = evidence / "answer.txt"
    answer.write_text("human answer\n", encoding="utf-8")
    dispatch_terminal = evidence / "dispatcher.terminal.json"
    dispatch_terminal.write_text(json.dumps({
        "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "dispatcher observed terminality",
    }), encoding="utf-8")
    semantic = evidence / "worker.semantic.json"
    semantic_value = {
        "id": "review-1", "attempt_id": "attempt-1", "kind": kind,
    }
    if kind == "complete":
        semantic_value.update({"summary": "worker result", "verdict": verdict} if verdict is not None else {"summary": "worker result"})
    elif kind == "question":
        semantic_value["question"] = "Need clarification"
    else:
        semantic_value["reason"] = "provider did not return a review"
    semantic.write_text(json.dumps(semantic_value), encoding="utf-8")
    dispatch = evidence / "dispatch.json"
    dispatch.write_text(json.dumps({
        "id": "review-1", "attempt_id": "attempt-1", "status": status,
        "exit": exit_code, "terminal_observed": True,
        "output_path": f"{prefix}answer.txt", "output_sha256": digest(answer),
        "terminal_artifact_path": f"{prefix}dispatcher.terminal.json",
        "terminal_artifact_sha256": digest(dispatch_terminal),
    }), encoding="utf-8")
    return {
        "id": "review-1",
        "dispatch_receipt": {"path": f"{prefix}dispatch.json", "digest": digest(dispatch)},
        "dispatch_terminal_artifact": {
            "path": f"{prefix}dispatcher.terminal.json", "digest": digest(dispatch_terminal),
        },
        "terminal_artifact": {"path": f"{prefix}worker.semantic.json", "digest": digest(semantic)},
        "worktree_receipt": None,
    }


def test_acceptance_requires_three_distinct_digest_bound_paths(tmp_path):
    reference = write_attempt(tmp_path)
    reference.pop("dispatch_terminal_artifact")

    result = accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert "dispatch_terminal_artifact" in result["reason"]

    aliased = write_attempt(tmp_path)
    aliased["dispatch_terminal_artifact"] = {
        "path": "./worker.semantic.json",
        "digest": aliased["terminal_artifact"]["digest"],
    }
    dispatch = json.loads((tmp_path / "dispatch.json").read_text())
    dispatch["terminal_artifact_path"] = "worker.semantic.json"
    dispatch["terminal_artifact_sha256"] = aliased["terminal_artifact"]["digest"]
    (tmp_path / "dispatch.json").write_text(json.dumps(dispatch))
    aliased["dispatch_receipt"]["digest"] = digest(tmp_path / "dispatch.json")
    rejected = accept_worker_outcome(tmp_path, aliased)

    assert rejected["status"] == "rejected"
    assert "distinct" in rejected["reason"]


def test_complete_semantic_result_requires_worker_verdict(tmp_path):
    reference = write_attempt(tmp_path, verdict=None)

    result = accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert "verdict" in result["reason"]


def test_invalid_semantic_verdict_is_not_certifying(tmp_path):
    reference = write_attempt(tmp_path, verdict="not-a-review-verdict")

    result = accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert "verdict" in result["reason"]


def test_empty_human_answer_is_not_digest_bound(tmp_path):
    reference = write_attempt(tmp_path)
    (tmp_path / "answer.txt").write_text("", encoding="utf-8")

    result = accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert result["reason"] == "human answer is empty"


def test_same_inode_evidence_is_not_distinct_even_when_paths_and_digests_match(tmp_path):
    reference = write_attempt(tmp_path)
    answer = tmp_path / "answer.txt"
    answer.write_text(json.dumps({
        "id": "review-1", "attempt_id": "attempt-1", "kind": "complete",
        "summary": "dispatcher observed terminality",
    }), encoding="utf-8")
    hardlink = tmp_path / "dispatcher.terminal.json"
    hardlink.unlink()
    hardlink.hardlink_to(answer)
    dispatch = json.loads((tmp_path / "dispatch.json").read_text())
    dispatch["output_sha256"] = digest(answer)
    dispatch["terminal_artifact_path"] = "dispatcher.terminal.json"
    dispatch["terminal_artifact_sha256"] = digest(hardlink)
    (tmp_path / "dispatch.json").write_text(json.dumps(dispatch), encoding="utf-8")
    reference["dispatch_receipt"]["digest"] = digest(tmp_path / "dispatch.json")
    reference["dispatch_terminal_artifact"] = {
        "path": "dispatcher.terminal.json", "digest": digest(hardlink),
    }

    result = accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert "hardlink" in result["reason"] or "same inode" in result["reason"] or "distinct" in result["reason"]


def test_symlink_and_absolute_answer_paths_cannot_escape_the_run_directory(tmp_path):
    reference = write_attempt(tmp_path)
    outside = tmp_path.parent / "outside-answer.txt"
    outside.write_text("outside\n", encoding="utf-8")
    answer = tmp_path / "answer.txt"
    answer.unlink()
    answer.symlink_to(outside)
    dispatch = json.loads((tmp_path / "dispatch.json").read_text())
    dispatch["output_path"] = str(answer)
    dispatch["output_sha256"] = digest(outside)
    (tmp_path / "dispatch.json").write_text(json.dumps(dispatch), encoding="utf-8")
    reference["dispatch_receipt"]["digest"] = digest(tmp_path / "dispatch.json")

    result = accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert "escapes" in result["reason"] or "symlink" in result["reason"]


@pytest.mark.parametrize("kind", ("question", "failed", "unavailable", "blocked"))
def test_incomplete_worker_outcomes_are_never_certifying(tmp_path, kind):
    reference = write_attempt(tmp_path, kind=kind)

    result = accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "accepted"
    assert result["certifying"] is False


def test_non_ok_complete_worker_outcome_is_never_certifying(tmp_path):
    reference = write_attempt(tmp_path, status="error", exit_code=0)

    result = accept_worker_outcome(tmp_path, reference)

    assert result["status"] == "rejected"
    assert "dispatcher terminal complete contradicts" in result["reason"]


@pytest.mark.parametrize(
    "target_name",
    ("answer.txt", "worker.semantic.json", "dispatch.json"),
)
@pytest.mark.parametrize("race_kind", ("rename", "hardlink"))
def test_barrier_controlled_evidence_race_rejects_certification(
    tmp_path, monkeypatch, target_name, race_kind,
):
    reference = write_attempt(tmp_path)
    target = tmp_path / target_name
    barrier = threading.Event()
    raced = False

    def race_after_read(path, _label):
        nonlocal raced
        if raced or path != target:
            return
        raced = True
        barrier.set()
        assert barrier.is_set()
        if race_kind == "rename":
            moved = target.with_name(target.name + ".moved")
            target.rename(moved)
            target.write_bytes(moved.read_bytes())
        else:
            alias = target.with_name(target.name + ".alias")
            alias.hardlink_to(target)

    monkeypatch.setattr(worker_outcome, "_after_file_read", race_after_read, raising=False)

    result = accept_worker_outcome(tmp_path, reference)

    assert raced is True
    assert result["status"] == "rejected"
    assert (
        "identity" in result["reason"]
        or "inode" in result["reason"]
        or "digest" in result["reason"]
        or "hardlink" in result["reason"]
    )


@pytest.mark.parametrize(
    "artifact",
    ("answer.txt", "dispatcher.terminal.json", "dispatch.json", "worker.semantic.json"),
)
@pytest.mark.parametrize("race_kind", ("rename", "symlink"))
def test_parent_directory_swap_during_host_acceptance_fails_closed(
    tmp_path, monkeypatch, artifact, race_kind,
):
    reference = write_attempt(tmp_path, directory="evidence")
    target = tmp_path / "evidence" / artifact
    outside = tmp_path / "outside"
    outside.mkdir()
    raced = False

    def swap_parent(path, _label):
        nonlocal raced
        if raced or path != target:
            return
        raced = True
        moved = tmp_path / "evidence.moved"
        path.parent.rename(moved)
        if race_kind == "symlink":
            path.parent.symlink_to(outside, target_is_directory=True)
        else:
            path.parent.mkdir()

    monkeypatch.setattr(worker_outcome, "_after_file_read", swap_parent)

    result = accept_worker_outcome(tmp_path, reference)

    assert raced is True
    assert result["status"] == "rejected"
    assert any(
        word in result["reason"]
        for word in ("symlink", "changed", "missing", "unreadable", "inode", "Not a directory")
    )


def test_host_acceptance_ignores_fabricated_workflow_certification_claim(tmp_path):
    fabricated = {
        "crossFamily": {"ran": True, "crossFamily": True, "certificationEligible": True},
        "routeReceipt": "",
        "terminalResult": "",
    }

    error, leg = run_dir_finalize._direct_worker_leg(
        tmp_path,
        {"id": "review-1", "status": "complete", "verdict": "pass"},
        {"path": "missing-route.json", "digest": "sha256:" + "0" * 64},
        fabricated,
        {"path": "missing-worker.json", "digest": "sha256:" + "0" * 64},
        None,
        "openai",
        "substantial",
    )

    assert leg is None
    assert error
    assert "missing" in error or "unreadable" in error or "digest" in error or "reference" in error


def test_workflow_emits_reviewed_patches_for_host_application_only():
    workflow = Path(__file__).parents[1] / "workflows" / "implement-run.js"
    text = workflow.read_text(encoding="utf-8")

    assert "acceptance:implementation-claim" not in text
    assert "state: 'executing'" in text
    assert "awaiting-host-certification/application" in text
    assert "machineGatePassed: false" in text
    for forbidden in (
        "apply:serial",
        "git apply",
        "autoApplyAllowed",
        "implementationCandidate",
        "outerChairCanonicalWorktree",
        "outerChairBaseRevision",
        "hostClaimContextAvailable",
        "CERTIFICATION_SCHEMA",
        "other-primary-certification",
    ):
        assert forbidden not in text


def test_workflow_fails_closed_before_host_apply_on_failed_checks_or_blockers():
    workflow = Path(__file__).parents[1] / "workflows" / "implement-run.js"
    text = workflow.read_text(encoding="utf-8")

    failure_guard = "if (!checksPass || blocking > 0)"
    guard_at = text.index(failure_guard)
    host_apply_at = text.index("phase('Host apply')")

    assert guard_at < host_apply_at
    failure_tail = text[guard_at:host_apply_at]
    assert "await failRun(" in failure_tail
    assert "state: 'failed'" in failure_tail
    assert "machineGatePassed: false" in failure_tail
    assert text.index("state: 'executing'", host_apply_at) > host_apply_at


def test_workflow_hands_off_review_paths_for_host_certification():
    workflow = Path(__file__).parents[1] / "workflows" / "implement-run.js"
    text = workflow.read_text(encoding="utf-8")

    handoff_at = text.index("// --- Phase 6: host apply handoff")
    handoff = text[handoff_at:]

    assert "awaiting-host-certification/application" in handoff
    assert "state: 'executing'" in handoff
    assert "machineGatePassed: false" in handoff
    assert "otherPrimaryRan" not in handoff


def test_workflow_host_application_wait_is_transport_metadata_not_delivery_state():
    workflow = Path(__file__).parents[1] / "workflows" / "implement-run.js"
    text = workflow.read_text(encoding="utf-8")
    handoff_at = text.index("// --- Phase 6: host apply handoff")
    handoff = text[handoff_at:]

    assert "state: 'executing'" in handoff
    assert "transport:" in handoff
    assert "handoff: 'awaiting-host-certification/application'" in handoff
    assert "state: 'awaiting-host-apply'" not in handoff


def test_claimed_implementation_cannot_use_an_unused_verifier(tmp_path):
    reference = write_attempt(tmp_path)

    result = accept_worker_outcome(tmp_path, reference, require_worktree_receipt=True)

    assert result["status"] == "rejected"
    assert "worktree_receipt" in result["reason"]


def test_attempted_noncomplete_leg_cannot_use_free_text_without_joined_evidence():
    review = {
        "id": "unavailable-1", "scope": "targeted", "lens": "provider",
        "family": "openai", "tier": "workhorse", "status": "unavailable",
        "substitution_for": "", "evidence": None, "reason": "provider unavailable",
        "verdict": "", "terminal_result": None, "wave": 0,
        "adapter": "codex", "model": "", "catalog_model": "",
        "route_receipt": None, "reviewer_id": "reviewer-1",
        "adapter_gate": "direct-cli",
    }
    plan = {
        "risk_tier": "routine", "chair_family": "", "concurrency_ceiling": 1,
        "reviews": [review], "panels": [],
    }

    errors = run_dir_finalize._validate_review_plan(plan)

    assert any("terminal_result" in error for error in errors)
    assert any("route_receipt" in error for error in errors)


def test_never_attempted_omitted_leg_remains_explicit_without_route_or_terminal():
    review = {
        "id": "omitted-1", "scope": "targeted", "lens": "provider",
        "family": "openai", "tier": "workhorse", "status": "omitted",
        "substitution_for": "targeted-lens", "evidence": None,
        "reason": "not attempted: no verifier available", "verdict": "",
        "terminal_result": None, "wave": 0, "adapter": "codex", "model": "",
        "catalog_model": "", "route_receipt": None, "reviewer_id": "reviewer-1",
        "adapter_gate": "direct-cli",
    }
    plan = {
        "risk_tier": "routine", "chair_family": "", "concurrency_ceiling": 1,
        "reviews": [review], "panels": [],
    }

    assert run_dir_finalize._validate_review_plan(plan) == []


def test_implement_workflow_has_no_model_source_application_boundary():
    workflow = Path(__file__).parents[1] / "workflows" / "implement-run.js"
    text = workflow.read_text(encoding="utf-8")

    assert "patches/" in text
    assert "state: 'executing'" in text
    assert "awaiting-host-certification/application" in text
    assert "machineGatePassed: false" in text
    assert "acceptance:implementation-claim" not in text
    assert "reviewerId" in text


def test_documented_direct_commands_bind_the_row_reviewer_id():
    root = Path(__file__).parents[1]
    for relative in ("workflows/implement-run.js", "workflows/cross-verify.js", "workflows/codebase-polish.js"):
        text = (root / relative).read_text(encoding="utf-8")
        assert "--reviewer-id" in text
    assert "--reviewer-id <reviewer-id>" not in (root / "workflows" / "cross-verify.js").read_text(encoding="utf-8")
    assert "cand.reviewerId" not in (root / "workflows" / "codebase-polish.js").read_text(encoding="utf-8")


def test_claim_acceptance_helper_is_the_executable_common_join():
    helper = Path(__file__).parents[1] / "skills" / "implement" / "scripts" / "accept_claim.py"
    text = helper.read_text(encoding="utf-8")

    assert "verify-claim" in text
    assert "WORKTREE_SCRIPT = ROOT / \"scripts\" / \"worktree.py\"" in text
    assert "RECEIPT_NAME" in text
    assert "RESULT_NAME" in text
    assert "args.repo" not in text
    assert "receipt_name" not in text
    assert "result_name" not in text


def test_claim_acceptance_helper_executes_verifier_and_binds_receipt(tmp_path, monkeypatch, capsys):
    run = tmp_path / "run"
    run.mkdir()
    canonical = tmp_path / "canonical"
    canonical.mkdir()
    command_calls = []
    expected_common = Path("/installed/common.git")
    monkeypatch.setattr(accept_claim.worktree_policy, "common_git_dir", lambda _path: expected_common)

    def fake_run(command, **kwargs):
        command_calls.append(command)
        return __import__("subprocess").CompletedProcess(
            command,
            0,
            stdout=json.dumps({
                "status": "accepted", "base_revision": "a" * 40,
                "head_revision": "b" * 40, "claimed_commit": "b" * 40, "clean": True,
                "claimed_worktree": str(canonical),
                "common_git_dir": str(expected_common),
            }),
            stderr="",
        )

    monkeypatch.setattr(accept_claim.subprocess, "run", fake_run)
    args = SimpleNamespace(
        canonical_worktree=canonical,
        claimed_worktree=None,
        claimed_commit="b" * 40,
        base_revision="a" * 40,
        run_dir=run,
    )

    assert accept_claim.accept(args) == 0
    result = json.loads(capsys.readouterr().out)
    assert result["status"] == "accepted"
    assert "verify-claim" in command_calls[0]
    assert str(accept_claim.WORKTREE_SCRIPT) == command_calls[0][1]
    assert command_calls[0][command_calls[0].index("--repo") + 1] == str(canonical)
    assert result["worktree_receipt"]["digest"].startswith("sha256:")
    assert result["acceptance_result"]["path"] == "implementation.acceptance.json"


def test_claim_acceptance_rejects_nonzero_installed_verifier_even_if_stdout_claims_success(
    tmp_path, monkeypatch, capsys,
):
    run = tmp_path / "run"
    run.mkdir()
    canonical = tmp_path / "canonical"
    canonical.mkdir()
    monkeypatch.setattr(accept_claim.worktree_policy, "common_git_dir", lambda _path: Path("/installed/common.git"))

    def fake_run(command, **kwargs):
        return __import__("subprocess").CompletedProcess(
            command, 7, stdout=json.dumps({"status": "accepted"}), stderr="verifier failed",
        )

    monkeypatch.setattr(accept_claim.subprocess, "run", fake_run)
    args = SimpleNamespace(
        canonical_worktree=canonical, claimed_worktree=None,
        claimed_commit="b" * 40, base_revision="a" * 40,
        run_dir=run,
    )

    assert accept_claim.accept(args) == 1
    result = json.loads(capsys.readouterr().out)
    assert result["status"] == "rejected"
    assert "verifier failed" in result["reason"]


def test_claim_acceptance_cli_has_no_worker_outcome_option():
    parser = accept_claim.parser()

    assert not any(
        "--outcome-reference" in action.option_strings
        for action in parser._actions
    )


def _claim_fixture(tmp_path):
    repo = tmp_path / "project"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(repo), "config", "user.name", "Test"], check=True)
    (repo / "tracked.txt").write_text("base\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "base"], check=True)
    base = subprocess.check_output(["git", "-C", str(repo), "rev-parse", "HEAD"], text=True).strip()
    worktree = repo / ".worktrees" / "implementation"
    worktree.parent.mkdir()
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "--detach", "-q", str(worktree), base], check=True)
    (worktree / "tracked.txt").write_text("implementation\n", encoding="utf-8")
    subprocess.run(["git", "-C", str(worktree), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(worktree), "commit", "-qm", "implementation"], check=True)
    claimed = subprocess.check_output(["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True).strip()
    return repo, worktree, base, claimed


def test_valid_new_clean_exact_linked_worktree_succeeds_only_through_direct_helper(tmp_path, capsys):
    _repo, worktree, base, claimed = _claim_fixture(tmp_path)
    run = tmp_path / "run"
    run.mkdir()
    run_receipt = run / "RUN.json"
    run_receipt.write_text('{"status":"executing"}\n', encoding="utf-8")
    args = SimpleNamespace(
        canonical_worktree=worktree, claimed_worktree=None,
        claimed_commit=claimed, base_revision=base,
        run_dir=run,
    )

    (run / "unrelated-worker-outcome.json").write_text(
        json.dumps({"id": "unrelated", "attempt_id": "attempt-unrelated", "kind": "complete"}),
        encoding="utf-8",
    )

    assert accept_claim.accept(args) == 0
    result = json.loads(capsys.readouterr().out)
    assert result["status"] == "accepted"
    assert result["result_revision"] == claimed
    assert json.loads(run_receipt.read_text())["status"] == "executing"
    assert (run / accept_claim.RECEIPT_NAME).is_file()
    assert (run / accept_claim.RESULT_NAME).is_file()
