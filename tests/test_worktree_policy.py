import importlib.util
import hashlib
import json
from pathlib import Path
import subprocess
import sys

import pytest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "worktree.py"
SPEC = importlib.util.spec_from_file_location("worktree_policy", SCRIPT)
worktree_policy = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
sys.modules[SPEC.name] = worktree_policy
SPEC.loader.exec_module(worktree_policy)


def init_repo(path: Path) -> str:
    path.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", str(path)], check=True)
    subprocess.run(["git", "-C", str(path), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(path), "config", "user.name", "Test"], check=True)
    (path / "tracked.txt").write_text("base\n")
    subprocess.run(["git", "-C", str(path), "add", "."], check=True)
    subprocess.run(["git", "-C", str(path), "commit", "-qm", "base"], check=True)
    return subprocess.check_output(["git", "-C", str(path), "rev-parse", "HEAD"], text=True).strip()


def test_authorised_detached_worktree_uses_shared_project_directory(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)

    assert worktree_policy.main([
        "create", "review-one", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0

    receipt = json.loads(capsys.readouterr().out)
    expected = repo / ".worktrees" / "review-one"
    assert Path(receipt["worktree_root"]) == expected
    assert receipt["head_revision"] == head
    assert receipt["branch"] is None
    assert receipt["detached"] is True
    assert expected.is_dir()
    assert subprocess.check_output(
        ["git", "-C", str(expected), "rev-parse", "--show-toplevel"], text=True,
    ).strip() == str(expected)


def test_creation_from_linked_checkout_still_anchors_primary_root(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    first = repo / ".worktrees" / "first"
    first.parent.mkdir()
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "--detach", str(first), head], check=True)

    assert worktree_policy.main([
        "create", "second", "--repo", str(first), "--detach", head, "--human-authorised",
    ]) == 0

    receipt = json.loads(capsys.readouterr().out)
    assert Path(receipt["primary_root"]) == repo
    assert Path(receipt["worktree_root"]) == repo / ".worktrees" / "second"
    assert not (first / ".worktrees").exists()


def test_creation_requires_authority_and_rejects_unsafe_names(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main(["create", "plain", "--repo", str(repo), "--detach", head]) == 2
    assert "explicit human authorisation" in capsys.readouterr().err
    assert worktree_policy.main([
        "create", "../escape", "--repo", str(repo), "--detach", head, "--human-authorised",
    ]) == 2
    assert "safe filename" in capsys.readouterr().err
    assert not (tmp_path / "escape").exists()


def test_new_branch_requires_separate_branch_authority(tmp_path, capsys):
    repo = tmp_path / "project"
    init_repo(repo)
    args = ["create", "feature", "--repo", str(repo), "--new-branch", "feature/test", "--human-authorised"]
    assert worktree_policy.main(args) == 2
    assert "branch requires separate" in capsys.readouterr().err
    assert worktree_policy.main(args + ["--branch-authorised"]) == 0
    receipt = json.loads(capsys.readouterr().out)
    branch = subprocess.check_output(
        ["git", "-C", str(repo / ".worktrees" / "feature"), "branch", "--show-current"], text=True,
    ).strip()
    assert branch == "feature/test"
    assert receipt["branch"] == "feature/test"
    assert receipt["detached"] is False


def test_ignore_rule_is_repository_local_and_idempotent(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    for name in ("one", "two"):
        assert worktree_policy.main([
            "create", name, "--repo", str(repo), "--detach", head, "--human-authorised",
        ]) == 0
        capsys.readouterr()
    exclude = repo / ".git" / "info" / "exclude"
    assert exclude.read_text().splitlines().count("/.worktrees/") == 1
    assert subprocess.run(
        ["git", "-C", str(repo), "check-ignore", "--no-index", ".worktrees/probe"],
        check=False,
    ).returncode == 0


def test_check_reports_only_direct_project_local_registered_worktrees(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main([
        "create", "valid", "--repo", str(repo), "--detach", head, "--human-authorised",
    ]) == 0
    capsys.readouterr()

    assert worktree_policy.main(["check", "--repo", str(repo)]) == 0
    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "pass"
    assert receipt["findings"] == []

    outside = tmp_path / "outside"
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "--detach", str(outside), head], check=True)
    assert worktree_policy.main(["check", "--repo", str(repo)]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "fail"
    assert any("outside canonical .worktrees" in finding for finding in receipt["findings"])


def test_symlinked_or_tracked_shared_root_is_rejected(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    (repo / ".worktrees").symlink_to(elsewhere, target_is_directory=True)
    assert worktree_policy.main([
        "create", "one", "--repo", str(repo), "--detach", head, "--human-authorised",
    ]) == 2
    assert "not a symlink" in capsys.readouterr().err

    (repo / ".worktrees").unlink()
    (repo / ".worktrees").mkdir()
    (repo / ".worktrees" / "notice.txt").write_text("tracked\n")
    subprocess.run(["git", "-C", str(repo), "add", "-f", ".worktrees/notice.txt"], check=True)
    subprocess.run(["git", "-C", str(repo), "commit", "-qm", "track reserved root"], check=True)
    assert worktree_policy.main([
        "create", "two", "--repo", str(repo), "--detach", "HEAD", "--human-authorised",
    ]) == 2
    assert "tracked paths" in capsys.readouterr().err


def test_remove_refuses_dirty_worktree_and_never_deletes_branch(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main([
        "create", "clean", "--repo", str(repo), "--detach", head, "--human-authorised",
    ]) == 0
    capsys.readouterr()
    target = repo / ".worktrees" / "clean"
    (target / "untracked.txt").write_text("preserve\n")
    assert worktree_policy.main([
        "remove", "clean", "--repo", str(repo), "--human-authorised",
    ]) == 2
    assert "worktree is dirty" in capsys.readouterr().err
    assert target.is_dir()
    (target / "untracked.txt").unlink()
    assert worktree_policy.main([
        "remove", "clean", "--repo", str(repo), "--human-authorised",
    ]) == 0
    assert not target.exists()


def _implementation_worktree(tmp_path, capsys):
    repo = tmp_path / "project"
    base = init_repo(repo)
    assert worktree_policy.main([
        "create", "implementation", "--repo", str(repo), "--detach", base,
        "--human-authorised",
    ]) == 0
    capsys.readouterr()
    return repo, base, repo / ".worktrees" / "implementation"


def test_verify_claim_requires_a_recorded_pre_dispatch_base(tmp_path, capsys):
    _repo, _base, worktree = _implementation_worktree(tmp_path, capsys)

    assert worktree_policy.main([
        "verify-claim", "--expected-worktree", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", "0" * 40,
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt == {"status": "rejected", "reason": "missing pre-dispatch base revision"}


def test_verify_claim_rejects_primary_and_wrong_linked_worktrees(tmp_path, capsys):
    repo = tmp_path / "project"
    base = init_repo(repo)
    first = repo / ".worktrees" / "first"
    second = repo / ".worktrees" / "second"
    first.parent.mkdir()
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "--detach", str(first), base], check=True)
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "--detach", str(second), base], check=True)

    primary = worktree_policy.verify_claim(repo, repo, base, base_revision=base)
    assert primary["status"] == "rejected"
    assert "canonical" in primary["reason"] or "linked" in primary["reason"]

    wrong_linked = worktree_policy.verify_claim(first, second, base, base_revision=base)
    assert wrong_linked == {
        "status": "rejected",
        "reason": "claimed worktree context does not match expected worktree",
    }
    capsys.readouterr()


def test_verify_claim_rejects_a_worktree_from_the_wrong_common_git_directory(tmp_path, capsys):
    _repo, base, worktree = _implementation_worktree(tmp_path, capsys)
    other = tmp_path / "other"
    other_base = init_repo(other)
    foreign = other / ".worktrees" / "foreign"
    foreign.parent.mkdir()
    subprocess.run(["git", "-C", str(other), "worktree", "add", "--detach", str(foreign), other_base], check=True)

    result = worktree_policy.verify_claim(worktree, foreign, other_base, base_revision=base)

    assert result == {
        "status": "rejected",
        "reason": "claimed worktree is from a different common Git directory",
    }


def test_verify_claim_rejects_a_non_descendant_head(tmp_path, capsys):
    _repo, base, worktree = _implementation_worktree(tmp_path, capsys)
    subprocess.run(["git", "-C", str(worktree), "checkout", "--orphan", "unrelated"], check=True, stdout=subprocess.DEVNULL)
    (worktree / "unrelated.txt").write_text("unrelated\n")
    subprocess.run(["git", "-C", str(worktree), "add", "unrelated.txt"], check=True)
    subprocess.run(["git", "-C", str(worktree), "commit", "-qm", "unrelated"], check=True)
    claimed = subprocess.check_output(["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True).strip()

    result = worktree_policy.verify_claim(worktree, worktree, claimed, base_revision=base)

    assert result == {
        "status": "rejected",
        "reason": "claimed commit is not descended from pre-dispatch base",
    }


def test_verify_claim_rejects_unchanged_base_and_dirty_residue(tmp_path, capsys):
    _repo, base, worktree = _implementation_worktree(tmp_path, capsys)
    (worktree / "tracked.txt").write_text("uncommitted\n")

    assert worktree_policy.main([
        "verify-claim", "--expected-worktree", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", base,
        "--base-revision", base,
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert "unchanged" in receipt["reason"]

    (worktree / "tracked.txt").unlink()
    (worktree / "lane-output.txt").write_text("uncommitted\n")
    changed = base
    assert worktree_policy.main([
        "verify-claim", "--expected-worktree", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", changed,
        "--base-revision", base,
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert "unchanged" in receipt["reason"]


def test_verify_claim_rejects_dirty_residue_against_a_new_claim(tmp_path, capsys):
    _repo, base, worktree = _implementation_worktree(tmp_path, capsys)
    (worktree / "tracked.txt").write_text("implementation\n")
    subprocess.run(["git", "-C", str(worktree), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(worktree), "commit", "-qm", "implementation"], check=True)
    claimed = subprocess.check_output(
        ["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True,
    ).strip()
    (worktree / "lane-output.txt").write_text("uncommitted\n")

    assert worktree_policy.main([
        "verify-claim", "--expected-worktree", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", claimed,
        "--base-revision", base,
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert "residue" in receipt["reason"]


def test_verify_claim_accepts_new_descended_clean_head_with_receipt_evidence(tmp_path, capsys):
    _repo, base, worktree = _implementation_worktree(tmp_path, capsys)
    (worktree / "tracked.txt").write_text("implementation\n")
    subprocess.run(["git", "-C", str(worktree), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(worktree), "commit", "-qm", "implementation"], check=True)
    claimed = subprocess.check_output(
        ["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True,
    ).strip()

    assert worktree_policy.main([
        "verify-claim", "--expected-worktree", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", claimed,
        "--base-revision", base,
    ]) == 0

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "accepted"
    assert receipt["base_revision"] == base
    assert receipt["head_revision"] == claimed
    assert receipt["clean"] is True
    assert receipt["claimed_commit"] == claimed


def test_verify_claim_allows_only_documented_generated_ignored_paths(tmp_path, capsys):
    _repo, base, worktree = _implementation_worktree(tmp_path, capsys)
    (worktree / "node_modules" / "generated.js").parent.mkdir()
    (worktree / "node_modules" / "generated.js").write_text("generated\n")

    assert worktree_policy.main([
        "verify-claim", "--expected-worktree", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", base,
        "--base-revision", base,
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert "unchanged" in receipt["reason"]

    (worktree / "tracked.txt").write_text("implementation\n")
    subprocess.run(["git", "-C", str(worktree), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(worktree), "commit", "-qm", "implementation"], check=True)
    claimed = subprocess.check_output(
        ["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True,
    ).strip()
    (worktree / ".env").write_text("unexpected\n")

    assert worktree_policy.main([
        "verify-claim", "--expected-worktree", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", claimed,
        "--base-revision", base,
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert "residue" in receipt["reason"]


def test_verify_claim_rejects_a_head_change_during_verification(tmp_path, capsys, monkeypatch):
    _repo, base, worktree = _implementation_worktree(tmp_path, capsys)
    (worktree / "tracked.txt").write_text("first\n")
    subprocess.run(["git", "-C", str(worktree), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(worktree), "commit", "-qm", "first"], check=True)
    claimed = subprocess.check_output(
        ["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True,
    ).strip()
    real_git = worktree_policy.git
    advanced = False

    def racing_git(repo_path, *args, **kwargs):
        nonlocal advanced
        result = real_git(repo_path, *args, **kwargs)
        if not advanced and args == ("rev-parse", "--verify", f"{claimed}^{{commit}}"):
            (worktree / "tracked.txt").write_text("second\n")
            subprocess.run(["git", "-C", str(worktree), "add", "tracked.txt"], check=True)
            subprocess.run(["git", "-C", str(worktree), "commit", "-qm", "second"], check=True)
            advanced = True
        return result

    monkeypatch.setattr(worktree_policy, "git", racing_git)
    assert worktree_policy.main([
        "verify-claim", "--expected-worktree", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", claimed,
        "--base-revision", base,
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert "advanced" in receipt["reason"]


def test_diagnostics_redact_token_only_and_incomplete_credential_urls(monkeypatch):
    secret = "proxy-token-for-review"
    monkeypatch.setenv("npm_config_proxy", secret)

    diagnostic = worktree_policy.redact_diagnostic(
        "https://:token-only-secret@example.test/ https://review-user:partial-secret/path"
        f" {secret}"
    )

    assert "token-only-secret" not in diagnostic
    assert "partial-secret" not in diagnostic
    assert secret not in diagnostic
    assert "https://[REDACTED]@example.test/" in diagnostic
    assert "https://[REDACTED]/path" in diagnostic


def test_capture_is_bounded_but_counts_and_digests_all_output():
    message = b"x" * (worktree_policy.MAX_DIAGNOSTIC_BYTES * 2 + 17)
    process = subprocess.Popen(
        [sys.executable, "-c", "import sys; sys.stdout.buffer.write(sys.argv[1].encode())", message.decode()],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    captures = worktree_policy._drain_process(process)
    capture = captures["stdout"]

    assert len(capture["buffer"]) <= worktree_policy.MAX_DIAGNOSTIC_BYTES
    assert capture["size"] == len(message)
    assert capture["digest"].hexdigest() == hashlib.sha256(message).hexdigest()


def test_capture_redacts_secret_prefix_at_exact_capture_boundary(monkeypatch):
    secret = "boundary-proxy-token-123456789"
    monkeypatch.setenv("npm_config_proxy", secret)
    message = b"x" * (worktree_policy.MAX_DIAGNOSTIC_BYTES - len(secret)) + secret.encode() + b"tail"
    process = subprocess.Popen(
        [sys.executable, "-c", "import sys; sys.stdout.buffer.write(sys.argv[1].encode())", message.decode()],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    captures = worktree_policy._drain_process(process)
    diagnostic = worktree_policy._capture_text(captures["stdout"])

    assert secret not in diagnostic
    assert secret[:8] not in diagnostic
    assert f"bytes={len(message)}" in diagnostic
    assert len(diagnostic.encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES


@pytest.mark.parametrize(
    "payload",
    [
        b"worktree /tmp/example\0HEAD deadbeef\0branch refs/heads/main\0",
        b"worktree /tmp/example\0HEAD deadbeef\0branch refs/heads/main\0\0unknown value\0\0",
    ],
)
def test_worktree_records_rejects_incomplete_or_malformed_porcelain(tmp_path, monkeypatch, payload):
    capture = {
        "buffer": bytearray(payload),
        "size": len(payload),
        "digest": hashlib.sha256(payload),
        "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
    }

    def fake_run_git(repo_path, *args, **kwargs):
        assert kwargs == {"stdout_limit": None}
        return (
            subprocess.CompletedProcess(
                ["git", "-C", str(repo_path), *args], 0, "", ""
            ),
            {
                "stdout": capture,
                "stderr": {
                    "buffer": bytearray(),
                    "size": 0,
                    "digest": hashlib.sha256(),
                    "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
                },
            },
        )

    monkeypatch.setattr(worktree_policy, "_run_git", fake_run_git)
    with pytest.raises(worktree_policy.PolicyError, match="(?:incomplete|malformed)"):
        worktree_policy.worktree_records(tmp_path)


def test_worktree_records_accepts_a_large_complete_porcelain_inventory(tmp_path, monkeypatch):
    path = "/tmp/" + "a" * (worktree_policy.MAX_DIAGNOSTIC_BYTES + 1)
    head = "1" * 40
    payload = f"worktree {path}\0HEAD {head}\0detached\0\0".encode()
    capture = {
        "buffer": bytearray(payload),
        "size": len(payload),
        "digest": hashlib.sha256(payload),
        "limit": None,
    }

    def fake_run_git(repo_path, *args, **kwargs):
        assert kwargs == {"stdout_limit": None}
        return (
            subprocess.CompletedProcess(
                ["git", "-C", str(repo_path), *args], 0, "", ""
            ),
            {
                "stdout": capture,
                "stderr": {
                    "buffer": bytearray(),
                    "size": 0,
                    "digest": hashlib.sha256(),
                    "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
                },
            },
        )

    monkeypatch.setattr(worktree_policy, "_run_git", fake_run_git)

    assert worktree_policy.worktree_records(tmp_path) == [
        {"worktree": path, "HEAD": head, "detached": True}
    ]


def test_git_launcher_oserror_is_reported_as_policy_error(tmp_path, monkeypatch):
    def fail_popen(*args, **kwargs):
        raise OSError("git launcher unavailable")

    monkeypatch.setattr(worktree_policy.subprocess, "Popen", fail_popen)

    with pytest.raises(worktree_policy.PolicyError, match="could not launch git"):
        worktree_policy.git(tmp_path, "status")


def test_git_is_found_outside_the_fallback_install_roots(tmp_path, monkeypatch):
    """A git installed by nix, asdf or a custom prefix must still be found.

    Pinning lookup to a fixed directory list failed closed on install layouts it
    did not anticipate, which docs/ASRS.md treats as a reliability regression
    rather than a safety feature.
    """
    custom_root = tmp_path / "nix" / "store" / "git" / "bin"
    custom_root.mkdir(parents=True)
    shim = custom_root / "git"
    shim.write_text("#!/bin/sh\nexit 0\n")
    shim.chmod(0o755)

    monkeypatch.setenv("PATH", str(custom_root))

    assert worktree_policy.git_executable() == str(shim)
    assert worktree_policy.git_environment()["PATH"] == str(custom_root)


def test_git_falls_back_to_usual_install_roots_when_path_is_empty(monkeypatch):
    monkeypatch.setenv("PATH", "")

    resolved = worktree_policy.git_executable()

    assert resolved.endswith("/git")
    assert worktree_policy.git_environment()["PATH"] == worktree_policy.fallback_tool_path()


def test_git_environment_keeps_porcelain_parsing_deterministic():
    environment = worktree_policy.git_environment()

    assert environment["GIT_CONFIG_NOSYSTEM"] == "1"
    assert environment["GIT_TERMINAL_PROMPT"] == "0"
