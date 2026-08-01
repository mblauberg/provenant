import importlib.util
import json
from pathlib import Path
import subprocess
import sys


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


def test_verify_claim_rejects_missing_commit_sha(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main([
        "create", "implementation", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0
    capsys.readouterr()
    worktree = repo / ".worktrees" / "implementation"

    assert worktree_policy.main([
        "verify-claim", "--repo", str(worktree),
        "--claimed-worktree", str(worktree),
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "rejected"
    assert receipt["reason"] == "missing claimed commit SHA"


def test_verify_claim_rejects_sha_from_wrong_worktree_context(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    for name in ("expected", "other"):
        assert worktree_policy.main([
            "create", name, "--repo", str(repo), "--detach", head,
            "--human-authorised",
        ]) == 0
        capsys.readouterr()
    expected = repo / ".worktrees" / "expected"
    other = repo / ".worktrees" / "other"
    (other / "tracked.txt").write_text("other change\n")
    subprocess.run(["git", "-C", str(other), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(other), "commit", "-qm", "other change"], check=True)
    other_head = subprocess.check_output(
        ["git", "-C", str(other), "rev-parse", "HEAD"], text=True,
    ).strip()

    assert worktree_policy.main([
        "verify-claim", "--repo", str(expected),
        "--claimed-worktree", str(other), "--claimed-commit", other_head,
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "rejected"
    assert "worktree context" in receipt["reason"]


def test_verify_claim_rejects_sha_from_wrong_repository(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main([
        "create", "implementation", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0
    capsys.readouterr()
    expected = repo / ".worktrees" / "implementation"

    foreign = tmp_path / "foreign"
    foreign_head = init_repo(foreign)

    assert worktree_policy.main([
        "verify-claim", "--repo", str(expected),
        "--claimed-worktree", str(foreign), "--claimed-commit", foreign_head,
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "rejected"
    assert "common Git directory" in receipt["reason"]


def test_verify_claim_rejects_registered_worktree_outside_canonical_root(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    outside = tmp_path / "outside-worktree"
    subprocess.run(
        ["git", "-C", str(repo), "worktree", "add", "--detach", str(outside), head],
        check=True,
    )

    assert worktree_policy.main([
        "verify-claim", "--repo", str(outside),
        "--claimed-worktree", str(outside), "--claimed-commit", head,
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "rejected"
    assert "canonical .worktrees" in receipt["reason"]


def test_verify_claim_rejects_symlinked_canonical_root(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    real_shared = tmp_path / "real-worktrees"
    real_shared.mkdir()
    (repo / ".worktrees").symlink_to(real_shared, target_is_directory=True)
    target = repo / ".worktrees" / "implementation"
    subprocess.run(
        ["git", "-C", str(repo), "worktree", "add", "--detach", str(target), head],
        check=True,
    )
    resolved_target = target.resolve()

    assert worktree_policy.main([
        "verify-claim", "--repo", str(resolved_target),
        "--claimed-worktree", str(resolved_target), "--claimed-commit", head,
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "rejected"
    assert "canonical .worktrees" in receipt["reason"]


def test_verify_claim_accepts_head_sha_from_expected_linked_worktree(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main([
        "create", "implementation", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0
    capsys.readouterr()
    worktree = repo / ".worktrees" / "implementation"
    (worktree / "tracked.txt").write_text("implementation change\n")
    subprocess.run(["git", "-C", str(worktree), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(worktree), "commit", "-qm", "implementation change"], check=True)
    claimed = subprocess.check_output(
        ["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True,
    ).strip()

    assert worktree_policy.main([
        "verify-claim", "--repo", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", claimed,
    ]) == 0

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "accepted"
    assert receipt["claimed_commit"] == claimed
    assert receipt["claimed_worktree"] == str(worktree)


def test_verify_claim_rejects_claim_when_worktree_head_advances_during_check(
    tmp_path, capsys, monkeypatch,
):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main([
        "create", "implementation", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0
    capsys.readouterr()
    worktree = repo / ".worktrees" / "implementation"
    (worktree / "tracked.txt").write_text("first implementation change\n")
    subprocess.run(["git", "-C", str(worktree), "add", "tracked.txt"], check=True)
    subprocess.run(["git", "-C", str(worktree), "commit", "-qm", "first implementation change"], check=True)
    claimed = subprocess.check_output(
        ["git", "-C", str(worktree), "rev-parse", "HEAD"], text=True,
    ).strip()
    (worktree / "tracked.txt").write_text("second implementation change\n")

    real_git = worktree_policy.git
    advanced = False

    def racing_git(repo_path, *args, **kwargs):
        nonlocal advanced
        result = real_git(repo_path, *args, **kwargs)
        if not advanced and args == ("rev-parse", "--verify", f"{claimed}^{{commit}}"):
            subprocess.run(["git", "-C", str(worktree), "add", "tracked.txt"], check=True)
            subprocess.run(
                ["git", "-C", str(worktree), "commit", "-qm", "second implementation change"],
                check=True,
            )
            advanced = True
        return result

    monkeypatch.setattr(worktree_policy, "git", racing_git)
    assert worktree_policy.main([
        "verify-claim", "--repo", str(worktree),
        "--claimed-worktree", str(worktree), "--claimed-commit", claimed,
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "rejected"
    assert "advanced" in receipt["reason"]
