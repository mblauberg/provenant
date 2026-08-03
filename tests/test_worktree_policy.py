import importlib.util
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

import pytest


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "worktree.py"
SPEC = importlib.util.spec_from_file_location("worktree_policy", SCRIPT)
worktree_policy = importlib.util.module_from_spec(SPEC)
assert SPEC.loader
sys.modules[SPEC.name] = worktree_policy
SPEC.loader.exec_module(worktree_policy)


def _add_provisioning_commands(
    path: Path,
    *,
    install_exit: int = 0,
    build_exit: int = 0,
    install_fail_once: bool = False,
    build_fail_once: bool = False,
    block_provision: bool = False,
    invoke_node_tools: bool = False,
) -> None:
    scripts = path / "scripts"
    scripts.mkdir()
    install_failure = (
        "if [ ! -f \"$root/.install-attempted\" ]; then\n"
        "  touch \"$root/.install-attempted\"\n"
        "  echo install failed >&2\n"
        "  exit 7\n"
        "fi\n"
        if install_fail_once
        else ""
    )
    build_failure = (
        "if [ ! -f \"$root/.build-attempted\" ]; then\n"
        "  touch \"$root/.build-attempted\"\n"
        "  echo build failed >&2\n"
        "  exit 9\n"
        "fi\n"
        if build_fail_once
        else ""
    )
    build_body = (
        "echo build failed >&2\nexit 9"
        if build_exit
        else "mkdir -p \"$root/runtime/agent-fabric-protocol/dist\"\n"
        "touch \"$root/runtime/agent-fabric-protocol/dist/index.js\""
    )
    provision_barrier = (
        "if [ -f \"$root/.provision-barrier\" ]; then\n"
        "  touch \"$root/.provision-entered\"\n"
        "  while [ ! -f \"$root/.provision-release\" ]; do sleep 0.01; done\n"
        "fi\n"
        if block_provision
        else ""
    )
    tool_probe = "npm --version >/dev/null\nnode --version >/dev/null\n" if invoke_node_tools else ""
    (scripts / "install-agent-fabric-dependencies").write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "root=$(CDPATH= cd -- \"$(dirname -- \"$0\")/..\" && pwd)\n"
        f"{provision_barrier}"
        "echo install >> \"$root/.provision-log\"\n"
        f"{tool_probe}"
        "if [ -n \"${PROVISION_SENTINEL:-}\" ]; then echo sentinel >> \"$root/.provision-log\"; fi\n"
        f"{install_failure}"
        "mkdir -p \"$root/node_modules\"\n"
        "mkdir -p \"$root/runtime/agent-fabric\"\n"
        ": > \"$root/runtime/agent-fabric/.npm-ci-attestation\"\n"
        f"{'echo install failed >&2' if install_exit else ''}\n"
        f"exit {install_exit}\n"
    )
    (scripts / "agent-fabric-protocol-build").write_text(
        "#!/bin/sh\n"
        "set -eu\n"
        "root=${AGENTS_HOME:-$(CDPATH= cd -- \"$(dirname -- \"$0\")/..\" && pwd)}\n"
        "echo build >> \"$root/.provision-log\"\n"
        "if [ -n \"${PROVISION_SENTINEL:-}\" ]; then echo sentinel >> \"$root/.provision-log\"; fi\n"
        "if [ -n \"${npm_config_proxy:-}\" ]; then echo \"proxy=$npm_config_proxy\" >&2; fi\n"
        f"{build_failure}"
        f"{build_body}\n"
        f"exit {build_exit}\n"
    )
    for script in scripts.iterdir():
        script.chmod(0o755)


def init_repo(
    path: Path,
    *,
    install_exit: int = 0,
    build_exit: int = 0,
    install_fail_once: bool = False,
    build_fail_once: bool = False,
    block_provision: bool = False,
    invoke_node_tools: bool = False,
) -> str:
    path.mkdir(parents=True)
    _add_provisioning_commands(
        path,
        install_exit=install_exit,
        build_exit=build_exit,
        install_fail_once=install_fail_once,
        build_fail_once=build_fail_once,
        block_provision=block_provision,
        invoke_node_tools=invoke_node_tools,
    )
    (path / ".gitignore").write_text(
        "node_modules/\n"
        "runtime/agent-fabric-protocol/dist/\n"
        ".provision-log\n"
        ".install-attempted\n"
        ".build-attempted\n"
        ".provision-entered\n"
        ".provision-release\n"
        "runtime/agent-fabric/.npm-ci-attestation\n"
    )
    if block_provision:
        (path / ".provision-barrier").write_text("hold\n")
    subprocess.run(["git", "init", "-q", str(path)], check=True)
    subprocess.run(["git", "-C", str(path), "config", "user.email", "test@example.com"], check=True)
    subprocess.run(["git", "-C", str(path), "config", "user.name", "Test"], check=True)
    (path / "tracked.txt").write_text("base\n")
    subprocess.run(["git", "-C", str(path), "add", "."], check=True)
    subprocess.run(["git", "-C", str(path), "commit", "-qm", "base"], check=True)
    return subprocess.check_output(["git", "-C", str(path), "rev-parse", "HEAD"], text=True).strip()


def wait_for_file(path: Path, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 10
    while not path.exists():
        if process.poll() is not None:
            raise AssertionError(process.communicate()[1])
        if time.monotonic() >= deadline:
            process.kill()
            raise AssertionError(f"timed out waiting for {path}")
        time.sleep(0.01)


def test_authorised_detached_worktree_uses_shared_project_directory(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)

    assert worktree_policy.main([
        "create", "review-one", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0

    receipt = json.loads(capsys.readouterr().out)
    expected = repo / ".worktrees" / "review-one"
    assert receipt["status"] == "ready"
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


def test_create_returns_ready_only_after_install_then_protocol_build(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)

    assert worktree_policy.main([
        "create", "ready", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0

    receipt = json.loads(capsys.readouterr().out)
    target = repo / ".worktrees" / "ready"
    assert receipt["status"] == "ready"
    assert receipt["provisioned_steps"] == ["dependency-installation", "protocol-build"]
    assert (target / "node_modules").is_dir()
    assert (target / "runtime/agent-fabric-protocol/dist/index.js").is_file()
    assert (target / ".provision-log").read_text().splitlines() == ["install", "build"]


def test_create_returns_dependency_install_failure_before_build(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo, install_exit=7)

    assert worktree_policy.main([
        "create", "install-failure", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "failed"
    assert receipt["failed_step"] == "dependency-installation"
    assert receipt["exit_code"] == 7
    assert receipt["provisioned_steps"] == []
    assert receipt["worktree_root"] == str(repo / ".worktrees" / "install-failure")
    assert (repo / ".worktrees" / "install-failure" / ".provision-log").read_text().splitlines() == ["install"]


def test_create_returns_protocol_build_failure_after_install(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo, build_exit=9)

    assert worktree_policy.main([
        "create", "build-failure", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "failed"
    assert receipt["failed_step"] == "protocol-build"
    assert receipt["exit_code"] == 9
    assert receipt["provisioned_steps"] == ["dependency-installation"]
    assert receipt["assumed_steps"] == []
    assert receipt["stderr"].startswith("[child output omitted; bytes=")
    assert (repo / ".worktrees" / "build-failure" / ".provision-log").read_text().splitlines() == ["install", "build"]


def test_repeated_create_does_not_reprovision_an_existing_worktree(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    args = [
        "create", "idempotent", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]

    assert worktree_policy.main(args) == 0
    capsys.readouterr()
    assert worktree_policy.main(args) == 2
    assert "already exists" in capsys.readouterr().err
    assert (repo / ".worktrees" / "idempotent" / ".provision-log").read_text().splitlines() == ["install", "build"]


def test_provisioning_does_not_inherit_unlisted_environment(tmp_path, capsys, monkeypatch):
    repo = tmp_path / "project"
    head = init_repo(repo)
    monkeypatch.setenv("PROVISION_SENTINEL", "secret")

    assert worktree_policy.main([
        "create", "closed-env", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0

    target = repo / ".worktrees" / "closed-env"
    assert "sentinel" not in (target / ".provision-log").read_text()
    capsys.readouterr()


def test_provisioning_uses_trusted_tool_path_not_caller_shims(tmp_path, capsys, monkeypatch):
    repo = tmp_path / "project"
    head = init_repo(repo, invoke_node_tools=True)
    fake_bin = tmp_path / "fake-bin"
    fake_bin.mkdir()
    for tool in ("npm", "node"):
        shim = fake_bin / tool
        shim.write_text(f"#!/bin/sh\ntouch '{tmp_path / (tool + '-shim-used')}'\nexit 99\n")
        shim.chmod(0o755)
    monkeypatch.setenv("PATH", str(fake_bin) + os.pathsep + os.environ.get("PATH", ""))

    assert worktree_policy.main([
        "create", "trusted-tools", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0
    capsys.readouterr()
    assert not (tmp_path / "npm-shim-used").exists()
    assert not (tmp_path / "node-shim-used").exists()


def test_git_failure_receipt_does_not_disclose_ambient_secret(tmp_path, capsys, monkeypatch):
    repo = tmp_path / "project"
    head = init_repo(repo)
    hook = repo / ".git" / "hooks" / "post-checkout"
    hook.write_text("#!/bin/sh\nprintf '%s\\n' \"$GITHUB_TOKEN\" >&2\nexit 1\n")
    hook.chmod(0o755)
    secret = "ambient-github-token-for-review"
    monkeypatch.setenv("GITHUB_TOKEN", secret)

    assert worktree_policy.main([
        "create", "secret-hook", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 2
    captured = capsys.readouterr()
    receipt = json.loads(captured.out)
    assert receipt["status"] == "failed"
    assert secret not in captured.out
    assert secret not in captured.err


def test_git_invalid_utf8_failure_still_returns_typed_receipt(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    hook = repo / ".git" / "hooks" / "post-checkout"
    hook.write_text("#!/bin/sh\nprintf '\\377' >&2\nexit 1\n")
    hook.chmod(0o755)

    assert worktree_policy.main([
        "create", "invalid-utf8", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "failed"
    assert receipt["failed_step"] == "git"
    assert receipt["exit_code"] != 0


def test_git_oversized_failure_diagnostic_is_bounded(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    hook = repo / ".git" / "hooks" / "post-checkout"
    hook.write_text("#!/bin/sh\nhead -c 20000 /dev/zero | tr '\\000' x >&2\nexit 1\n")
    hook.chmod(0o755)

    assert worktree_policy.main([
        "create", "large-diagnostic", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "failed"
    assert len(receipt["stderr"].encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES
    assert "[truncated; bytes=" in receipt["stderr"]
    assert "sha256=" in receipt["stderr"]


def test_failed_git_receipt_redacts_credential_bearing_command_arguments(tmp_path, capsys):
    repo = tmp_path / "project"
    init_repo(repo)
    revision = "https://review-user:revision-secret@example.test/missing"

    assert worktree_policy.main([
        "create", "credential-revision", "--repo", str(repo), "--detach", revision,
        "--human-authorised",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "failed"
    assert "revision-secret" not in json.dumps(receipt)
    assert receipt["command"][-1] == "https://[REDACTED]@example.test/missing"


def test_failed_git_receipt_redacts_token_only_credential_arguments(tmp_path, capsys):
    repo = tmp_path / "project"
    init_repo(repo)
    revision = "https://:token-only-secret@example.test/missing"

    assert worktree_policy.main([
        "create", "token-only-credential", "--repo", str(repo), "--detach", revision,
        "--human-authorised",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)

    assert "token-only-secret" not in json.dumps(receipt)
    assert receipt["command"][-1] == "https://[REDACTED]@example.test/missing"


@pytest.mark.parametrize("suffix", ["", "\n"])
def test_diagnostics_redact_incomplete_credential_urls(suffix):
    value = f"https://review-user:partial-secret{suffix}"

    assert "partial-secret" not in worktree_policy.bounded_diagnostic(value)
    assert "partial-secret" not in worktree_policy.bounded_command(["git", value])[-1]


def test_provision_retries_failed_install_then_returns_ready(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo, install_fail_once=True)
    args = ["create", "retry-install", "--repo", str(repo), "--detach", head, "--human-authorised"]

    assert worktree_policy.main(args) == 2
    capsys.readouterr()
    assert worktree_policy.main([
        "provision", "retry-install", "--repo", str(repo), "--human-authorised",
        "--from-step", "dependency-installation",
    ]) == 0

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "ready"
    assert receipt["provisioned_steps"] == ["dependency-installation", "protocol-build"]
    assert receipt["assumed_steps"] == []
    assert (repo / ".worktrees" / "retry-install" / ".provision-log").read_text().splitlines() == [
        "install", "install", "build",
    ]


def test_provision_retries_failed_build_without_reinstalling(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo, build_fail_once=True)
    args = ["create", "retry-build", "--repo", str(repo), "--detach", head, "--human-authorised"]

    assert worktree_policy.main(args) == 2
    capsys.readouterr()
    assert worktree_policy.main([
        "provision", "retry-build", "--repo", str(repo), "--human-authorised",
        "--from-step", "protocol-build",
    ]) == 0

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "ready"
    assert receipt["provisioned_steps"] == ["protocol-build"]
    assert receipt["assumed_steps"] == ["dependency-installation"]
    assert (repo / ".worktrees" / "retry-build" / ".provision-log").read_text().splitlines() == [
        "install", "build", "build",
    ]


def test_provision_rejects_registered_symlink_target_outside_shared_root(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    external = tmp_path / "external"
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "--detach", str(external), head], check=True)
    shared = repo / ".worktrees"
    shared.mkdir()
    (shared / "escape").symlink_to(external, target_is_directory=True)

    assert worktree_policy.main([
        "provision", "escape", "--repo", str(repo), "--human-authorised",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert "outside canonical" in receipt["stderr"]
    assert not (external / ".provision-log").exists()


def test_failed_receipt_redacts_allowed_proxy_value(tmp_path, capsys, monkeypatch):
    repo = tmp_path / "project"
    head = init_repo(repo, build_exit=9)
    monkeypatch.setenv("npm_config_proxy", "proxy-token-for-review")

    assert worktree_policy.main([
        "create", "redacted", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 2

    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "failed"
    assert "proxy-token-for-review" not in receipt["stderr"]
    assert receipt["stderr"].startswith("[child output omitted; bytes=")


def test_branch_identity_git_failure_keeps_git_metadata_in_receipt(tmp_path, capsys, monkeypatch):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main([
        "create", "branch-failure", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0
    capsys.readouterr()

    real_git = worktree_policy.git

    def fail_symbolic_ref(repo_path, *args, check=True):
        if args == ("symbolic-ref", "--quiet", "--short", "HEAD"):
            return subprocess.CompletedProcess(
                args=["git", "-C", str(repo_path), *args],
                returncode=128,
                stdout="",
                stderr="symbolic-ref probe failed",
            )
        return real_git(repo_path, *args, check=check)

    monkeypatch.setattr(worktree_policy, "git", fail_symbolic_ref)
    assert worktree_policy.main([
        "provision", "branch-failure", "--repo", str(repo), "--human-authorised",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "failed"
    assert receipt["operation"] == "provision"
    assert receipt["failed_step"] == "git"
    assert receipt["command"] == [
        "git", "-C", str(repo / ".worktrees" / "branch-failure"),
        "symbolic-ref", "--quiet", "--short", "HEAD",
    ]
    assert receipt["exit_code"] == 128
    assert receipt["stderr"] == "symbolic-ref probe failed"


def test_retry_holds_exclusive_writer_lock_across_provisioning(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main([
        "create", "locked", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0
    capsys.readouterr()
    target = repo / ".worktrees" / "locked"

    with worktree_policy.exclusive_provision_lock(target):
        result = subprocess.run(
            [
                str(SCRIPT), "provision", "locked", "--repo", str(repo),
                "--human-authorised",
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        remove_result = subprocess.run(
            [
                str(SCRIPT), "remove", "locked", "--repo", str(repo),
                "--human-authorised",
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
    assert result.returncode == 2
    assert json.loads(result.stdout)["stderr"] == "worktree provisioning is already in progress"
    assert remove_result.returncode == 2
    assert "already in progress" in remove_result.stderr


def test_build_only_retry_requires_install_attestation(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    assert worktree_policy.main([
        "create", "attestation", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 0
    capsys.readouterr()
    target = repo / ".worktrees" / "attestation"
    (target / "runtime/agent-fabric/.npm-ci-attestation").unlink()

    assert worktree_policy.main([
        "provision", "attestation", "--repo", str(repo), "--human-authorised",
        "--from-step", "protocol-build",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert receipt["failed_step"] == "provision-gate"
    assert "prior npm install attestation" in receipt["stderr"]


def test_admission_reservation_excludes_same_name_only(tmp_path):
    repo = tmp_path / "project"
    init_repo(repo)

    with worktree_policy.admission_reservation(repo, "same"):
        admission_dir = worktree_policy.common_git_dir(repo) / worktree_policy.ADMISSION_DIRECTORY
        assert (admission_dir / "same.lock").is_file()
        assert not (admission_dir / "other.lock").exists()
        second = subprocess.run(
            [sys.executable, str(SCRIPT), "create", "same", "--repo", str(repo),
             "--detach", "HEAD", "--human-authorised"],
            text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
        )

    assert second.returncode == 2
    assert json.loads(second.stdout)["stderr"] == "worktree name is already admitted"


def test_different_names_can_overlap_during_initial_provisioning(tmp_path):
    repo = tmp_path / "project"
    head = init_repo(repo, block_provision=True)
    first_target = repo / ".worktrees" / "first"
    second_target = repo / ".worktrees" / "second"

    first = subprocess.Popen(
        [
            str(SCRIPT), "create", "first", "--repo", str(repo),
            "--detach", head, "--human-authorised",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    wait_for_file(first_target / ".provision-entered", first)

    second = subprocess.Popen(
        [
            str(SCRIPT), "create", "second", "--repo", str(repo),
            "--detach", head, "--human-authorised",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    wait_for_file(second_target / ".provision-entered", second)

    (first_target / ".provision-release").touch()
    (second_target / ".provision-release").touch()
    first_stdout, first_stderr = first.communicate(timeout=10)
    second_stdout, second_stderr = second.communicate(timeout=10)

    assert first.returncode == 0, first_stderr
    assert second.returncode == 0, second_stderr
    assert json.loads(first_stdout)["status"] == "ready"
    assert json.loads(second_stdout)["status"] == "ready"


def test_real_process_diagnostic_redacts_before_capture_boundary(monkeypatch):
    secret = "process-boundary-proxy-token-123456789"
    monkeypatch.setenv("npm_config_proxy", secret)
    retained_size = worktree_policy.MAX_DIAGNOSTIC_BYTES - len("\n[truncated]")
    message = "x" * (retained_size - 8) + secret + "tail" * 100
    process = subprocess.Popen(
        [sys.executable, "-c", "import sys; sys.stdout.write(sys.argv[1])", message],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    captures = worktree_policy._drain_process(process)
    diagnostic = worktree_policy._capture_text(captures["stdout"])

    assert captures["stdout"]["size"] == len(message.encode())
    assert secret not in diagnostic
    assert secret[:8] not in diagnostic
    assert len(diagnostic.encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES


def test_real_process_capture_is_bounded_but_counts_and_digests_all_output():
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


def test_capture_diagnostic_preserves_exact_marker_when_utf8_expands():
    message = b"\xff" * worktree_policy.MAX_DIAGNOSTIC_BYTES
    process = subprocess.Popen(
        [sys.executable, "-c", "import sys; sys.stdout.buffer.write(bytes([255]) * int(sys.argv[1]))", str(len(message))],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    capture = worktree_policy._drain_process(process)["stdout"]
    diagnostic = worktree_policy._capture_text(capture)

    assert f"bytes={len(message)}" in diagnostic
    assert f"sha256={hashlib.sha256(message).hexdigest()}" in diagnostic
    assert len(diagnostic.encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES


def test_capture_diagnostic_redacts_secret_prefix_at_exact_capture_cap(monkeypatch):
    secret = "exact-cap-configured-secret-987654321"
    monkeypatch.setenv("npm_config_registry", secret)
    prefix = secret[:12]
    raw = ("x" * (worktree_policy.MAX_DIAGNOSTIC_BYTES - len(prefix)) + prefix).encode()
    capture = {
        "buffer": bytearray(raw),
        "size": len(raw),
        "digest": hashlib.sha256(raw),
        "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
    }

    diagnostic = worktree_policy._capture_text(capture)

    assert prefix not in diagnostic
    assert len(diagnostic.encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES


@pytest.mark.parametrize("key", ["npm_config_proxy", "npm_config_registry"])
@pytest.mark.parametrize("extra", [b"", b"z" * 17])
def test_capture_diagnostic_redacts_unicode_secret_prefix_at_capture_boundary(monkeypatch, key, extra):
    secret = "a" * 1000 + "é"
    monkeypatch.setenv(key, secret)
    prefix = secret.encode()[:-1]
    raw = (b"x" * (worktree_policy.MAX_DIAGNOSTIC_BYTES - len(prefix)) + prefix + extra)
    capture = {
        "buffer": bytearray(raw[:worktree_policy.MAX_DIAGNOSTIC_BYTES]),
        "size": len(raw),
        "digest": hashlib.sha256(raw),
        "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
    }

    diagnostic = worktree_policy._capture_text(capture)

    assert "a" * 32 not in diagnostic
    assert len(diagnostic.encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES


def test_capture_diagnostic_redacts_credential_url_at_exact_capture_cap():
    secret = "exact-cap-url-secret-987654321"
    fragment = f"https://review-user:{secret}-{'s' * 80}"
    raw = ("x" * (worktree_policy.MAX_DIAGNOSTIC_BYTES - len(fragment)) + fragment).encode()
    capture = {
        "buffer": bytearray(raw),
        "size": len(raw),
        "digest": hashlib.sha256(raw),
        "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
    }

    diagnostic = worktree_policy._capture_text(capture)

    assert secret not in diagnostic
    assert secret[:8] not in diagnostic
    assert len(diagnostic.encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES


@pytest.mark.parametrize("extra", [b"", b"tail" * 17])
def test_capture_diagnostic_redacts_url_prefix_at_capture_boundary(extra):
    fragment = b"https://credential-user-secret-" + b"u" * 64
    raw = b"x" * (worktree_policy.MAX_DIAGNOSTIC_BYTES - len(fragment)) + fragment + extra
    capture = {
        "buffer": bytearray(raw[:worktree_policy.MAX_DIAGNOSTIC_BYTES]),
        "size": len(raw),
        "digest": hashlib.sha256(raw),
        "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
    }

    diagnostic = worktree_policy._capture_text(capture)

    assert b"credential-user-secret" not in diagnostic.encode()
    assert len(diagnostic.encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES


@pytest.mark.parametrize("key", ["npm_config_proxy", "npm_config_registry"])
def test_capture_diagnostic_handles_surrogate_secret(key, monkeypatch):
    secret = "surrogate-secret-\udcff"
    monkeypatch.setitem(os.environ, key, secret)
    raw = b"prefix " + secret.encode(errors="surrogateescape") + b" suffix"
    capture = {
        "buffer": bytearray(raw),
        "size": len(raw),
        "digest": hashlib.sha256(raw),
        "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
    }

    diagnostic = worktree_policy._capture_text(capture)

    assert "surrogate-secret-" not in diagnostic
    assert len(diagnostic.encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES


def test_git_failure_receipt_redacts_partial_credential_url_at_capture_boundary(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)
    secret = "boundary-url-secret-987654321"
    url = f"https://review-user:{secret}-{'s' * 240}@registry.example.test/path"
    payload = "x" * (worktree_policy.MAX_DIAGNOSTIC_BYTES - 142) + url + "tail" * 100
    hook = repo / ".git" / "hooks" / "post-checkout"
    hook.write_text(f"#!/bin/sh\nprintf '%s' '{payload}' >&2\nexit 1\n")
    hook.chmod(0o755)

    assert worktree_policy.main([
        "create", "partial-url", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)

    assert receipt["failed_step"] == "git"
    assert secret not in json.dumps(receipt)
    assert secret[:8] not in json.dumps(receipt)
    assert "sha256=" in receipt["stderr"]


def test_worktree_records_rejects_bounded_porcelain_overflow(tmp_path, monkeypatch):
    repo = tmp_path / "project"
    overflow = b"worktree /project/.worktrees/overflow\0" + b"x" * worktree_policy.MAX_DIAGNOSTIC_BYTES
    command = ["git", "-C", str(repo), "worktree", "list", "--porcelain", "-z"]

    def fake_run_git(repo_path, *args):
        assert repo_path == repo
        assert args == ("worktree", "list", "--porcelain", "-z")
        return (
            subprocess.CompletedProcess(command, 0, "", ""),
            {
                "stdout": {
                    "buffer": bytearray(overflow[:worktree_policy.MAX_DIAGNOSTIC_BYTES]),
                    "size": len(overflow),
                    "digest": hashlib.sha256(overflow),
                    "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
                },
                "stderr": {
                    "buffer": bytearray(),
                    "size": 0,
                    "digest": hashlib.sha256(),
                    "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
                },
            },
        )

    monkeypatch.setattr(worktree_policy, "_run_git", fake_run_git)

    with pytest.raises(worktree_policy.PolicyError) as raised:
        worktree_policy.worktree_records(repo)

    assert raised.value.failed_step == "git"
    assert raised.value.command == command
    assert raised.value.exit_code == 0
    assert f"bytes={len(overflow)}" in raised.value.stdout
    assert f"sha256={hashlib.sha256(overflow).hexdigest()}" in raised.value.stdout
    assert len(raised.value.stdout.encode()) <= worktree_policy.MAX_DIAGNOSTIC_BYTES


def test_worktree_records_rejects_incomplete_porcelain_result(tmp_path, monkeypatch):
    repo = tmp_path / "project"
    incomplete = b"worktree /project/.worktrees/partial"
    command = ["git", "-C", str(repo), "worktree", "list", "--porcelain", "-z"]

    def fake_run_git(repo_path, *args):
        assert repo_path == repo
        assert args == ("worktree", "list", "--porcelain", "-z")
        return (
            subprocess.CompletedProcess(command, 0, "", ""),
            {
                "stdout": {
                    "buffer": bytearray(incomplete),
                    "size": len(incomplete),
                    "digest": hashlib.sha256(incomplete),
                    "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
                },
                "stderr": {
                    "buffer": bytearray(),
                    "size": 0,
                    "digest": hashlib.sha256(),
                    "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
                },
            },
        )

    monkeypatch.setattr(worktree_policy, "_run_git", fake_run_git)

    with pytest.raises(worktree_policy.PolicyError) as raised:
        worktree_policy.worktree_records(repo)

    assert raised.value.failed_step == "git"
    assert raised.value.command == command
    assert raised.value.exit_code == 0
    assert "incomplete" in str(raised.value)


@pytest.mark.parametrize(
    "malformed",
    [
        b"garbage\0\0",
        b"\0\0",
        b"\0\0worktree /one\0HEAD abc\0detached\0\0",
        b"worktree /one\0detached\0\0",
        b"worktree\0\0",
        b"worktree /one\0worktree /two\0\0",
        b"worktree /one\0HEAD abc\0detached\0\0\0\0worktree /two\0HEAD def\0detached\0\0",
    ],
)
def test_worktree_records_rejects_malformed_porcelain(tmp_path, monkeypatch, malformed):
    repo = tmp_path / "project"
    command = ["git", "-C", str(repo), "worktree", "list", "--porcelain", "-z"]

    def fake_run_git(repo_path, *args):
        assert repo_path == repo
        assert args == ("worktree", "list", "--porcelain", "-z")
        return (
            subprocess.CompletedProcess(command, 0, "", ""),
            {
                "stdout": {
                    "buffer": bytearray(malformed),
                    "size": len(malformed),
                    "digest": hashlib.sha256(malformed),
                    "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
                },
                "stderr": {
                    "buffer": bytearray(),
                    "size": 0,
                    "digest": hashlib.sha256(),
                    "limit": worktree_policy.MAX_DIAGNOSTIC_BYTES,
                },
            },
        )

    monkeypatch.setattr(worktree_policy, "_run_git", fake_run_git)

    with pytest.raises(worktree_policy.PolicyError) as raised:
        worktree_policy.worktree_records(repo)

    assert raised.value.failed_step == "git"
    assert raised.value.command == command
    assert raised.value.exit_code == 0
    assert "malformed" in str(raised.value)


def test_git_launcher_oserror_is_typed_as_git_failure(tmp_path, capsys, monkeypatch):
    repo = tmp_path / "project"
    head = init_repo(repo)

    def fail_popen(*args, **kwargs):
        raise OSError("git launcher unavailable")

    monkeypatch.setattr(worktree_policy.subprocess, "Popen", fail_popen)

    assert worktree_policy.main([
        "create", "launcher-failure", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)

    assert receipt["status"] == "failed"
    assert receipt["failed_step"] == "git"
    assert receipt["command"] == [
        "git", "-C", str(repo), "rev-parse", "--show-toplevel",
    ]
    assert receipt["exit_code"] is None
    assert "git launcher unavailable" in receipt["stderr"]


def test_expected_create_provision_remove_and_git_failures_are_typed(tmp_path, capsys):
    repo = tmp_path / "project"
    head = init_repo(repo)

    assert worktree_policy.main(["create", "unauthorised", "--repo", str(repo), "--detach", head]) == 2
    create_receipt = json.loads(capsys.readouterr().out)
    assert create_receipt["status"] == "failed"
    assert create_receipt["operation"] == "create"
    assert create_receipt["failed_step"] == "policy-gate"
    assert create_receipt["command"] == []
    assert create_receipt["exit_code"] is None

    assert worktree_policy.main([
        "provision", "missing", "--repo", str(repo), "--human-authorised",
    ]) == 2
    provision_receipt = json.loads(capsys.readouterr().out)
    assert provision_receipt["status"] == "failed"
    assert provision_receipt["operation"] == "provision"
    assert provision_receipt["failed_step"] == "policy-gate"
    assert provision_receipt["worktree_root"] == str(repo / ".worktrees" / "missing")

    assert worktree_policy.main([
        "create", "dirty", "--repo", str(repo), "--detach", head, "--human-authorised",
    ]) == 0
    capsys.readouterr()
    (repo / ".worktrees" / "dirty" / "untracked.txt").write_text("preserve\n")
    assert worktree_policy.main([
        "remove", "dirty", "--repo", str(repo), "--human-authorised",
    ]) == 2
    remove_receipt = json.loads(capsys.readouterr().out)
    assert remove_receipt["status"] == "failed"
    assert remove_receipt["operation"] == "remove"
    assert remove_receipt["failed_step"] == "policy-gate"
    assert remove_receipt["worktree_root"] == str(repo / ".worktrees" / "dirty")

    assert worktree_policy.main([
        "create", "git-failure", "--repo", str(repo), "--detach", "missing-revision",
        "--human-authorised",
    ]) == 2
    git_receipt = json.loads(capsys.readouterr().out)
    assert git_receipt["status"] == "failed"
    assert git_receipt["operation"] == "create"
    assert git_receipt["failed_step"] == "git"
    assert git_receipt["command"][:2] == ["git", "-C"]
    assert git_receipt["exit_code"] != 0


def test_check_ignore_git_failure_keeps_git_metadata_in_receipt(tmp_path, capsys, monkeypatch):
    repo = tmp_path / "project"
    head = init_repo(repo)
    real_git = worktree_policy.git

    def fail_check_ignore(repo_path, *args, check=True):
        if args == ("check-ignore", "--no-index", ".worktrees/.probe"):
            return subprocess.CompletedProcess(
                args=["git", "-C", str(repo_path), *args],
                returncode=128,
                stdout="probe stdout",
                stderr="check-ignore failed",
            )
        return real_git(repo_path, *args, check=check)

    monkeypatch.setattr(worktree_policy, "git", fail_check_ignore)
    assert worktree_policy.main([
        "create", "probe-failure", "--repo", str(repo), "--detach", head,
        "--human-authorised",
    ]) == 2
    receipt = json.loads(capsys.readouterr().out)
    assert receipt["status"] == "failed"
    assert receipt["operation"] == "create"
    assert receipt["failed_step"] == "git"
    assert receipt["command"] == [
        "git", "-C", str(repo), "check-ignore", "--no-index", ".worktrees/.probe",
    ]
    assert receipt["exit_code"] == 128
    assert receipt["stdout"] == "probe stdout"
    assert receipt["stderr"] == "check-ignore failed"


def test_diagnostics_redact_secrets_before_byte_boundary_truncation(monkeypatch):
    retained_size = worktree_policy.MAX_DIAGNOSTIC_BYTES - len("\n[truncated]")

    secret = "boundary-proxy-token-123456789"
    monkeypatch.setenv("npm_config_proxy", secret)
    message = "x" * (retained_size - 8) + secret + "tail" * 100

    bounded = worktree_policy.bounded_diagnostic(message)

    assert secret not in bounded
    assert secret[:8] not in bounded
    assert "[REDACTE" in bounded
    assert bounded.endswith("\n[truncated]")
    assert len(bounded.encode("utf-8")) <= worktree_policy.MAX_DIAGNOSTIC_BYTES

    registry_secret = "boundary-registry-token-987654321"
    monkeypatch.setenv("npm_config_registry", registry_secret)
    registry_message = "x" * (retained_size - 8) + registry_secret + "tail" * 100
    bounded_registry = worktree_policy.bounded_diagnostic(registry_message)
    assert registry_secret[:8] not in bounded_registry
    assert "[REDACTE" in bounded_registry

    url = "https://review-user:boundary-url-token-987654321@registry.example.test/path"
    bounded_url = worktree_policy.bounded_diagnostic(
        "x" * (retained_size - 16) + url + "tail" * 100,
    )
    assert "boundary-url-token-987654321" not in bounded_url
    assert "boundary-url-token-987654321"[:8] not in bounded_url
    assert "[REDACTE" in bounded_url


def test_create_admission_reservation_spans_initial_provisioning_and_remove(tmp_path):
    repo = tmp_path / "project"
    head = init_repo(repo, block_provision=True)
    target = repo / ".worktrees" / "barrier-remove"
    create_process = subprocess.Popen(
        [
            str(SCRIPT), "create", "barrier-remove", "--repo", str(repo),
            "--detach", head, "--human-authorised",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    wait_for_file(target / ".provision-entered", create_process)

    remove_result = subprocess.run(
        [
            str(SCRIPT), "remove", "barrier-remove", "--repo", str(repo),
            "--human-authorised",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert remove_result.returncode == 2
    remove_receipt = json.loads(remove_result.stdout)
    assert remove_receipt["operation"] == "remove"
    assert remove_receipt["failed_step"] == "policy-gate"
    assert "name is already admitted" in remove_receipt["stderr"]
    assert target.is_dir()

    (target / ".provision-release").touch()
    create_stdout, create_stderr = create_process.communicate(timeout=10)
    assert create_process.returncode == 0, create_stderr
    assert json.loads(create_stdout)["status"] == "ready"
    assert target.is_dir()


def test_create_admission_reservation_rejects_same_name_until_initial_provisioning_finishes(tmp_path):
    repo = tmp_path / "project"
    head = init_repo(repo, block_provision=True)
    target = repo / ".worktrees" / "barrier-create"
    create_process = subprocess.Popen(
        [
            str(SCRIPT), "create", "barrier-create", "--repo", str(repo),
            "--detach", head, "--human-authorised",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    wait_for_file(target / ".provision-entered", create_process)

    second_create = subprocess.run(
        [
            str(SCRIPT), "create", "barrier-create", "--repo", str(repo),
            "--detach", head, "--human-authorised",
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    assert second_create.returncode == 2
    second_receipt = json.loads(second_create.stdout)
    assert second_receipt["operation"] == "create"
    assert second_receipt["failed_step"] == "policy-gate"
    assert "name is already admitted" in second_receipt["stderr"]

    (target / ".provision-release").touch()
    create_stdout, create_stderr = create_process.communicate(timeout=10)
    assert create_process.returncode == 0, create_stderr
    assert json.loads(create_stdout)["status"] == "ready"
    assert (target / ".provision-log").read_text().splitlines() == ["install", "build"]
