"""Provider supervisor gates use fixture programs only, never model calls."""

import importlib
import json
import os
from pathlib import Path
import shlex
import shutil
import re
import signal
import socket
import stat
import subprocess
import ctypes
import sys
import tempfile
import time
import tomllib
from datetime import UTC, datetime, timedelta
from types import SimpleNamespace

import pytest
from tests.support.git import git as run_git, git_repo, linked_worktree as make_linked_worktree

from tests.support.confinement import force_sandbox

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "skills/orchestrate/scripts"


def provider_home(monkeypatch, tmp_path, *, home=None, config_dir=None, state_root=None,
                  instance_root=None):
    """Set only the provider home variables requested by this test."""
    values = {
        "HOME": home,
        "XDG_CONFIG_HOME": config_dir,
        "AGENT_FABRIC_STATE_ROOT": state_root,
        "AGENT_FABRIC_INSTANCE_ROOT": instance_root,
    }
    for name, value in values.items():
        if value is not None:
            monkeypatch.setenv(name, str(value))
    return Path(home) if home is not None else None


def test_plan_only_resolves_without_launch(tmp_path):
    provider = tmp_path / "claude"
    provider.write_text('#!/bin/sh\ntouch "' + str(tmp_path / "launched") + '"\n')
    provider.chmod(0o755)
    env = {
        **os.environ,
        "PATH": str(tmp_path) + ":" + os.environ["PATH"],
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
    }
    result = subprocess.run(
        [
            str(SCRIPTS / "cf_dispatch.py"),
            "--intent",
            "ordinary",
            "--tool",
            "claude",
            "--alias",
            "senior",
            "--prompt",
            "Hello",
            "--plan-only",
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr + result.stdout
    plan = json.loads(result.stdout)
    assert plan["schema"] == "fabric.exec-plan.v1"
    assert Path(plan["run_dir"]) != Path(plan["output_path"]).parent
    assert "--bare" not in plan["argv"]
    assert "--no-session-persistence" not in plan["argv"]
    assert "--session-id" in plan["argv"]
    assert "--permission-prompts" in plan["argv"]
    assert "stream-json" in plan["argv"]
    assert not (tmp_path / "launched").exists()


def test_read_only_os_confinement_profile_and_argv(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / "workspace"
    cwd = root / "sub"
    add_dir = tmp_path / "extra"
    cwd.mkdir(parents=True)
    add_dir.mkdir()
    xdg = tmp_path / "xdg"
    monkeypatch.setenv("XDG_CONFIG_HOME", str(xdg))
    plan = {
        "adapter": "agy", "mode": "read_only", "workspace_root": str(root),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]},
    }
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    profile = supervisor.provider_confinement.os_confinement_profile(plan)
    assert '(deny file-write*)' in profile
    assert '(deny file-read-data (subpath "' + str(root) + '")' in profile
    assert '(subpath "' + str(Path.home() / ".claude/projects") + '")' in profile
    assert '(subpath "' + str(Path.home() / ".codex/sessions") + '")' in profile
    git_config_rule = (
        f'(allow file-read-data (literal "{Path.home() / ".gitconfig"}") '
        f'(literal "{xdg / "git/config"}"))'
    )
    assert git_config_rule in profile
    assert '(allow file-read-data (subpath "' + str(cwd) + '") (subpath "' + str(add_dir) + '"))' in profile
    monkeypatch.setattr(supervisor.provider_confinement, "os_confinement_profile", lambda _plan, _search_path=None: "(version 1)")
    assert supervisor.provider_confinement.confinement_command(plan, ["/bin/cat", "file"]) == [
        "/usr/bin/sandbox-exec", "-p", "(version 1)", "/bin/cat", "file",
    ]


def test_read_only_git_config_allowlist_is_limited_to_the_two_global_files(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    home = (tmp_path / "home").resolve()
    home.mkdir()
    xdg = tmp_path / "xdg" / "config"
    monkeypatch.setattr(supervisor.Path, "home", lambda: home)
    monkeypatch.setenv("XDG_CONFIG_HOME", str(xdg))
    plan = {
        "adapter": "opencode", "mode": "read_only", "workspace_root": str(tmp_path / "workspace"),
        "cwd": str(tmp_path / "workspace"), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }
    profile = supervisor.provider_confinement.os_confinement_profile(plan)

    assert f'(literal "{home / ".gitconfig"}")' in profile
    assert f'(literal "{xdg / "git/config"}")' in profile
    assert f'(literal "{home / ".ssh/config"}")' not in profile
    # OpenCode v2 stats the home ancestry: home stays denied, discovery is re-allowed by literal.
    assert f'(subpath "{home}")' in profile
    assert f'(literal "{(tmp_path).resolve()}")' in profile
    assert f'(literal "{home / ".claude"}")' in profile
    assert f'(literal "{home / ".agents/AGENTS.md"}")' in profile
    assert f'(literal "{tmp_path.resolve() / "AGENTS.md"}")' in profile
    assert f'(subpath "{home / ".opencode"}")' in profile
    outside = supervisor.provider_confinement.os_confinement_profile({**plan, "cwd": "/private/tmp", "workspace_root": "/private/tmp"})
    assert f'(literal "{home}")' in outside
    other = supervisor.provider_confinement.os_confinement_profile({**plan, "adapter": "gemini"})
    assert f'(subpath "{home}")' in other

    monkeypatch.delenv("XDG_CONFIG_HOME")
    default_profile = supervisor.provider_confinement.os_confinement_profile(plan)
    assert f'(literal "{home / ".config/git/config"}")' in default_profile


def test_opencode_read_only_grants_configured_instruction_files(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    home = (tmp_path / "home").resolve()
    product = home / "Repos/provenant"
    product.mkdir(parents=True)
    (product / "HARNESS.md").write_text("constitution", encoding="utf-8")
    (home / ".ssh").mkdir()
    (home / ".ssh/id_ed25519").write_text("key", encoding="utf-8")
    config = home / ".config/opencode"
    config.mkdir(parents=True)
    (config / "HARNESS.md").symlink_to(product / "HARNESS.md")
    (config / "opencode.json").write_text(
        json.dumps({
            "$schema": "https://opencode.ai/config.json",
            "instructions": [str(config / "HARNESS.md"), str(home / ".ssh/id_ed25519"), "docs/*.md"],
        }),
        encoding="utf-8",
    )
    monkeypatch.setattr(supervisor.Path, "home", lambda: home)
    monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    workspace = (tmp_path / "elsewhere").resolve()
    workspace.mkdir()
    plan = {
        "adapter": "opencode", "mode": "read_only", "workspace_root": str(workspace),
        "cwd": str(workspace), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }
    profile = supervisor.provider_confinement.os_confinement_profile(plan)

    assert f'(literal "{product / "HARNESS.md"}")' in profile
    assert f'(literal "{product}")' in profile
    assert f'(literal "{home / "Repos"}")' in profile
    assert "id_ed25519" not in profile


def test_read_only_state_link_into_a_credential_store_is_not_granted(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    home = (tmp_path / "home").resolve()
    (home / ".ssh").mkdir(parents=True)
    (home / ".opencode").symlink_to(home / ".ssh")
    monkeypatch.setattr(supervisor.Path, "home", lambda: home)
    workspace = (tmp_path / "workspace").resolve()
    workspace.mkdir()
    plan = {
        "adapter": "opencode", "mode": "read_only", "workspace_root": str(workspace),
        "cwd": str(workspace), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }
    profile = supervisor.provider_confinement.os_confinement_profile(plan)

    assert f'(subpath "{home / ".opencode"}")' not in profile
    assert f'(subpath "{home / ".ssh"}")' not in profile
    assert f'(subpath "{home / ".config/opencode"}")' in profile


def test_opencode_read_only_discovery_skips_symlinks_to_credentials(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    home = (tmp_path / "home").resolve()
    (home / ".ssh").mkdir(parents=True)
    key = home / ".ssh/id_ed25519"
    key.write_text("secret")
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    (tmp_path / "AGENTS.md").symlink_to(key)
    real = tmp_path / "real" / "CLAUDE.md"
    real.parent.mkdir()
    real.write_text("ok")
    (tmp_path / "CLAUDE.md").symlink_to(real)
    monkeypatch.setattr(supervisor.Path, "home", lambda: home)
    plan = {
        "adapter": "opencode", "mode": "read_only", "workspace_root": str(workspace),
        "cwd": str(workspace), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }
    profile = supervisor.provider_confinement.os_confinement_profile(plan)
    assert "id_ed25519" not in profile
    assert f'(literal "{real.resolve()}")' in profile


@pytest.mark.parametrize("adapter", ["agy", "claude", "cursor", "opencode", "kiro"])
def test_writer_confinement_allows_owned_paths_and_add_dirs(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    primary, worktree = sandbox_profile_worktree(tmp_path)
    attempt = tmp_path / "run" / "attempt"
    common = primary / ".git"
    private = common / "worktrees/lane"
    add_dir = tmp_path / "extra"
    for path in (attempt, add_dir):
        path.mkdir(parents=True)
    plan = {"adapter": adapter, "mode": "worktree_write", "cwd": str(worktree),
            "workspace_root": str(worktree), "run_dir": str(attempt), "lane_ref": "refs/heads/lane",
            "trusted_git": {"common": str(common)},
            "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]}}
    profile = mod.provider_confinement.os_confinement_profile(plan)
    assert "(deny file-write*)" in profile
    assert f'(deny file-write* (subpath "{common}"))' in profile
    allow = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-write* "))
    for path in (worktree, add_dir, attempt, private, Path("/dev")):
        assert f'(subpath "{path}")' in allow
    assert f'(subpath "{common}")' not in allow
    assert f'(subpath "{common / "objects"}")' in allow
    for path in (common / "refs/heads/lane", common / "refs/heads/lane.lock", common / "logs/refs/heads/lane"):
        assert f'(literal "{path}")' in allow
    # Writers move only their own branch; the primary branch, upstreams and tags stay owner-only.
    for path in (common / "config", common / "hooks", common / "HEAD", common / "index", common / "refs",
                 common / "logs", common / "packed-refs", common / "refs/heads/main"):
        assert f'(subpath "{path}")' not in profile
        assert f'(literal "{path}")' not in profile
    for path in (Path.home() / ".cache", Path.home() / "Library/Caches", Path("/private/tmp")):
        assert f'(subpath "{path}")' not in profile
    deny = "\n".join(line for line in profile.splitlines() if line.startswith("(deny file-write* "))
    for path in (worktree / ".git", private / "commondir", private / "gitdir", private / "config.worktree",
                 common / "objects/info"):
        assert f'(subpath "{path}")' in deny
    for state_path in mod.provider_confinement.CONFINED_STATE[adapter]["read_write"]:
        assert mod.provider_confinement._sbpl_filter(Path.home() / state_path) in profile
    assert f'(subpath "{tmp_path}")' not in profile
    assert '(subpath "/dev")' in profile
    assert "(deny file-read-data" not in profile


def sandbox_profile_worktree(tmp_path):
    primary = git_repo(tmp_path / "primary", branch="main", commit=False)
    run_git(primary, "config", "user.name", "t")
    run_git(primary, "config", "user.email", "a@b")
    run_git(primary, "commit", "-q", "--allow-empty", "-m", "init")
    worktree = make_linked_worktree(primary, "lane", path=primary / ".worktrees/lane")
    return primary, worktree


@pytest.mark.parametrize("adapter", ["claude", "cursor", "opencode", "kiro"])
def test_writer_profile_grants_project_locks_only(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    primary, worktree = sandbox_profile_worktree(tmp_path)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    monkeypatch.setattr(mod, "_require_kiro_engine", lambda: None)
    plan = mod.build_plan(adapter, {}, "hello", cwd=worktree, workspace_root=worktree,
                          worktree=worktree, mode="worktree_write")
    locks = primary / ".agent-run/locks"
    assert locks.is_dir()
    assert plan["applied"]["confinement"] == "sandbox-exec"
    assert plan["applied"]["locks_dir"] == str(locks)
    assert str(locks) not in plan["applied"]["add_dirs"]
    assert str(locks) in plan["applied"]["write_boundary"]["writable_paths"]
    allow = "\n".join(line for line in mod.provider_confinement.os_confinement_profile(plan).splitlines()
                      if line.startswith("(allow file-write* "))
    assert f'(subpath "{locks}")' in allow
    for name in ("sessions", "scratch", "runs"):
        assert str(primary / ".agent-run" / name) not in allow
    assert f'(subpath "{primary / ".agent-run"}")' not in allow


def test_codex_writer_native_filesystem_grants_project_locks_and_runs_only(monkeypatch, tmp_path):
    mod = supervisor()
    primary, worktree = sandbox_profile_worktree(tmp_path)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = mod.build_plan("codex", {}, "hello", cwd=worktree, workspace_root=worktree,
                          worktree=worktree, mode="worktree_write")
    locks = str(primary / ".agent-run/locks")
    filesystem = plan["applied"]["write_boundary"]["filesystem"]
    assert filesystem[locks] == "write"
    assert locks not in plan["applied"]["add_dirs"]
    assert not [path for path in filesystem if path.startswith(str(primary / ".agent-run"))
                and path not in {locks, str(primary / ".agent-run/runs")}]
    assert filesystem[str(primary / ".agent-run/runs")] == "write"


def test_swapped_locks_link_is_not_granted_on_resume(monkeypatch, tmp_path):
    mod = supervisor()
    primary, worktree = sandbox_profile_worktree(tmp_path)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    first = mod.build_plan("claude", {}, "hello", cwd=worktree, workspace_root=worktree,
                           worktree=worktree, mode="worktree_write")
    target = tmp_path / "elsewhere"
    target.mkdir()
    locks = primary / ".agent-run/locks"
    locks.rmdir()
    locks.symlink_to(target)
    again = mod.build_plan("claude", {}, "hello", cwd=worktree, workspace_root=worktree,
                           worktree=worktree, mode="worktree_write",
                           add_dirs=first["applied"]["add_dirs"])
    assert again["applied"]["locks_dir"] is None
    assert any("not grantable" in item for item in again["warnings"])
    assert str(target) not in json.dumps(again["applied"])
    # A plan built before the swap must not reach the link target either.
    assert str(target) not in mod.provider_confinement.os_confinement_profile(first)
    assert not any(target.iterdir())


def test_writer_without_worktree_creates_no_locks(monkeypatch, tmp_path):
    mod = supervisor()
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = mod.build_plan("claude", {}, "hello", cwd=tmp_path, mode="worktree_write")
    assert plan["applied"]["locks_dir"] is None
    assert not (tmp_path / ".agent-run").exists()


@pytest.mark.skipif(not shutil.which("sandbox-exec"), reason="sandbox-exec unavailable")
def test_sandbox_probe_locks_writable_sessions_and_scratch_denied(monkeypatch, tmp_path):
    mod = supervisor()
    primary, worktree = sandbox_profile_worktree(tmp_path)
    for name in ("sessions", "scratch"):
        (primary / ".agent-run" / name).mkdir(parents=True)
    plan = mod.build_plan("claude", {}, "hello", cwd=worktree, workspace_root=worktree,
                          worktree=worktree, mode="worktree_write",
                          run_dir=tmp_path / "attempt")
    profile = mod.provider_confinement.os_confinement_profile(plan)

    def touch(directory):
        return subprocess.run(["sandbox-exec", "-p", profile, "/usr/bin/touch",
                               str(primary / ".agent-run" / directory / "probe")],
                              capture_output=True, text=True).returncode

    assert touch("locks") == 0
    assert touch("sessions") != 0
    assert touch("scratch") != 0


def test_read_only_plan_gets_no_locks_grant(monkeypatch, tmp_path):
    mod = supervisor()
    primary, worktree = sandbox_profile_worktree(tmp_path)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = mod.build_plan("claude", {}, "hello", cwd=worktree, workspace_root=worktree)
    assert not (primary / ".agent-run").exists()
    assert plan["applied"]["locks_dir"] is None


def test_writer_protected_path_inside_add_dir_keeps_read_and_write_denied(monkeypatch, tmp_path):
    mod = supervisor()
    _primary, worktree = sandbox_profile_worktree(tmp_path)
    add_dir = tmp_path / "extra"
    protected = add_dir / "private"
    protected.mkdir(parents=True)
    plan = {"adapter": "agy", "mode": "worktree_write", "cwd": str(worktree),
            "workspace_root": str(worktree), "protected_paths": [str(protected)],
            "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]}}

    profile = mod.provider_confinement.os_confinement_profile(plan)

    assert f'(allow file-write* (subpath "{worktree}") (subpath "{add_dir}")' in profile
    assert profile.rfind(f'(deny file-write* (subpath "{protected}"))') > profile.rfind(
        '(allow file-write* ')
    assert profile.rstrip().endswith(f'(deny file-read* (subpath "{protected}"))')


def test_writer_rejects_primary_checkout(tmp_path):
    mod = supervisor()
    subprocess.run(["git", "init", "-q", str(tmp_path)], check=True)
    with pytest.raises(ValueError, match="create a linked worktree"):
        mod.build_plan("claude", {}, "hello", workspace_root=tmp_path,
                       mode="worktree_write", worktree=tmp_path)


def test_writer_confinement_selection_and_degraded_warning(monkeypatch, tmp_path):
    mod = supervisor()
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    claude = mod.build_plan("claude", {}, "hello", cwd=tmp_path, mode="worktree_write")
    codex = mod.build_plan("codex", {}, "hello", cwd=tmp_path, mode="worktree_write")
    assert claude["applied"]["confinement"] == "sandbox-exec"
    assert codex["applied"]["confinement"] == "provider-native"
    name, overrides = codex_profile(codex)
    assert f'permissions.{name}.extends=":workspace"' in overrides
    force_sandbox(monkeypatch, None)
    degraded = mod.build_plan("claude", {}, "hello", cwd=tmp_path, mode="worktree_write")
    assert degraded["applied"]["confinement"] == "none"
    assert any("worktree_write writes are unconfined" in item for item in degraded["warnings"])


def test_agy_writer_requires_os_confinement_and_uses_write_profile(monkeypatch, tmp_path):
    mod = supervisor()
    add_dir = tmp_path.parent / "agy-extra"
    add_dir.mkdir()
    force_sandbox(monkeypatch, None)
    with pytest.raises(ValueError, match="sandbox-exec"):
        mod.build_plan("agy", {}, "hello", cwd=tmp_path, mode="worktree_write")

    (tmp_path / ".agents").mkdir()
    (tmp_path / ".agents/fabric-policy.json").write_text(
        '{"protected_paths":["secret/"]}'
    )
    probes = iter(("/usr/bin/sandbox-exec", None))
    force_sandbox(monkeypatch, lambda: next(probes))
    with pytest.raises(ValueError, match="sandbox-exec"):
        mod.build_plan("agy", {}, "hello", cwd=tmp_path, mode="worktree_write")

    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = mod.build_plan("agy", {}, "hello", cwd=tmp_path, mode="worktree_write",
                          add_dirs=[add_dir])
    assert plan["applied"]["confinement"] == "sandbox-exec"
    assert "--dangerously-skip-permissions" in plan["argv"]
    assert "--sandbox" not in plan["argv"]
    assert plan["protected_paths"] == [str(tmp_path / "secret")]
    profile = mod.provider_confinement.os_confinement_profile(plan)
    assert "(deny file-write*)" in profile
    assert f'(allow file-write* (subpath "{tmp_path}")' in profile
    assert f'(subpath "{add_dir}")' in "\n".join(
        line for line in profile.splitlines() if line.startswith("(allow file-write* ")
    )
    assert str(add_dir) in plan["applied"]["write_boundary"]["writable_paths"]
    assert profile.rstrip().endswith(
        f'(deny file-read* (subpath "{tmp_path / "secret"}"))'
    )
    force_sandbox(monkeypatch, None)
    with pytest.raises(RuntimeError, match="sandbox-exec"):
        mod.provider_confinement.confinement_command(plan, plan["argv"])


@pytest.mark.parametrize("adapter", ["agy", "opencode", "claude", "cursor", "kiro"])
def test_wrapped_writer_uses_only_provider_cache(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    profile = mod.provider_confinement.os_confinement_profile({
        "adapter": adapter, "mode": "worktree_write", "cwd": str(tmp_path),
        "workspace_root": str(tmp_path), "run_dir": str(tmp_path / "run"), "applied": {},
    })
    for path in (Path.home() / ".cache", Path.home() / "Library/Caches", Path("/private/tmp")):
        assert f'(subpath "{path}")' not in profile
    for path in mod.provider_confinement.CONFINED_STATE[adapter]["read_write"]:
        assert mod.provider_confinement._sbpl_filter(Path.home() / path) in profile


def protected_repo(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    (repo / ".agents").mkdir()
    (repo / ".agents/fabric-policy.json").write_text('{"protected_paths":["private/"]}')
    (repo / "private").mkdir()
    (repo / "safe").mkdir()
    return repo


def test_workspace_root_policy_resolves_outside_git(monkeypatch, tmp_path):
    mod = supervisor()
    workspace = tmp_path / "workspace"
    (workspace / ".agents").mkdir(parents=True)
    (workspace / ".agents/fabric-policy.json").write_text('{"protected_paths":["secret/"]}')
    (workspace / "secret").mkdir()
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = mod.build_plan("opencode", {"trains_on_prompts": True}, "hello",
                          cwd=workspace, workspace_root=workspace)
    assert plan["protected_paths"] == [str(workspace / "secret")]
    with pytest.raises(ValueError, match="protected path"):
        mod.build_plan("opencode", {"trains_on_prompts": True}, "hello",
                       cwd=workspace / "secret", workspace_root=workspace)


@pytest.mark.parametrize("declaration", ["{", "[]", '{"protected_paths":"secret/"}',
                                           '{"protected_paths":["../secret/"]}'])
def test_invalid_workspace_policy_refuses_training_route(tmp_path, declaration):
    mod = supervisor()
    (tmp_path / ".agents").mkdir()
    (tmp_path / ".agents/fabric-policy.json").write_text(declaration)
    with pytest.raises(ValueError, match="invalid protected path policy"):
        mod.build_plan("opencode", {"trains_on_prompts": True}, "hello",
                       cwd=tmp_path, workspace_root=tmp_path)


def test_policy_without_protected_paths_declares_none(tmp_path):
    mod = supervisor()
    (tmp_path / ".agents").mkdir()
    (tmp_path / ".agents/fabric-policy.json").write_text('{"memory_floor_percent":{"read_only":5}}')
    assert mod.provider_confinement.protected_paths(tmp_path) == []


def test_writer_discovers_worktree_policy_outside_workspace(monkeypatch, tmp_path):
    mod = supervisor()
    repo = protected_repo(tmp_path)
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    (repo / "private/fixture.txt").write_text("fixture", encoding="utf-8")
    subprocess.run(["git", "-C", str(repo), "add", ".agents/fabric-policy.json", "private/fixture.txt"], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c",
                    "user.email=test@example.invalid", "commit", "-q", "-m", "initial"], check=True)
    linked = tmp_path / "linked"
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "-q", "-b", "linked", str(linked)], check=True)
    extra = tmp_path / "extra"
    extra.mkdir()
    plan = mod.build_plan("opencode", {"trains_on_prompts": True}, "hello",
                          workspace_root=workspace, mode="worktree_write", worktree=linked,
                          add_dirs=[str(extra)])
    assert str(repo / "private") in plan["protected_paths"]
    boundary = plan["applied"]["write_boundary"]
    assert str(extra) in boundary["writable_paths"]
    assert str(repo / ".git/config") not in boundary["writable_paths"]
    assert str(repo / ".git/objects") in boundary["writable_paths"]


def test_non_git_workspace_discovers_child_repo_policy_and_linked_worktree(monkeypatch, tmp_path):
    mod = supervisor()
    workspace = tmp_path / "workspace"
    repo = workspace / "app"
    repo.mkdir(parents=True)
    git_repo(repo, branch="main")
    linked = make_linked_worktree(repo, "linked", path=repo / ".worktrees/linked")
    (repo / ".agents").mkdir()
    (repo / ".agents/fabric-policy.json").write_text('{"protected_paths":["secret/"]}')
    for root in (repo, linked):
        (root / "secret").mkdir()
        (root / "secret/prompt.md").write_text("sensitive")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = mod.build_plan("opencode", {"trains_on_prompts": True}, "hello",
                          cwd=workspace, workspace_root=workspace)
    profile = mod.provider_confinement.os_confinement_profile(plan)
    deny = next(line for line in profile.splitlines() if line.startswith("(deny file-read* "))
    for root in (repo, linked):
        assert f'(subpath "{root / "secret"}")' in deny
    monkeypatch.chdir(workspace)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(ROOT))
    for root in (repo, linked):
        task = {"id": "one", "adapter": "opencode", "model": "opencode/mimo-v2.6-flash-free",
                "prompt_file": str(root / "secret/prompt.md"), "cwd": str(workspace)}
        result = importlib.import_module("skills.orchestrate.scripts.dispatch_preflight").preflight_tasks([task])
        assert result["status"] == "rejected"
        assert "secret" in result["fix"] and "non-training route" in result["fix"]


def test_protected_profile_covers_registered_worktrees_and_non_training_route(monkeypatch, tmp_path):
    mod = supervisor()
    repo = protected_repo(tmp_path)
    sibling = tmp_path / "linked"
    original_run = mod.subprocess.run

    def listed(command, *args, **kwargs):
        if command[3:] == ["worktree", "list", "--porcelain"]:
            return SimpleNamespace(returncode=0, stdout=f"worktree {repo}\n\nworktree {sibling}\n")
        return original_run(command, *args, **kwargs)

    monkeypatch.setattr(mod.subprocess, "run", listed)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = mod.build_plan("opencode", {"trains_on_prompts": True}, "hello",
                          cwd=repo / "safe", workspace_root=repo)
    profile = mod.provider_confinement.os_confinement_profile(plan)
    deny = next(line for line in profile.splitlines() if line.startswith("(deny file-read* "))
    for root in (repo, sibling):
        assert f'(subpath "{root / "private"}")' in deny
    assert plan["applied"]["confinement"] == "sandbox-exec"
    assert plan["applied"]["protected_paths"] == plan["protected_paths"]
    assert str(repo / "private") in plan["applied"]["protected_paths"]
    safe = mod.build_plan("claude", {"trains_on_prompts": False}, "hello",
                          cwd=repo / "safe", workspace_root=repo)
    assert safe["protected_paths"] == []
    assert safe["applied"]["protected_paths"] == []
    assert safe["applied"]["confinement"] == "sandbox-exec"


@pytest.mark.parametrize("field", ["prompt_file", "add_dirs", "cwd"])
def test_protected_preflight_refuses_training_inputs(monkeypatch, tmp_path, field):
    repo = protected_repo(tmp_path)
    monkeypatch.chdir(repo)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(ROOT))
    task = {"id": "one", "adapter": "opencode", "model": "opencode/mimo-v2.6-flash-free",
            "prompt": "hello", "cwd": str(repo / "safe")}
    if field == "prompt_file":
        task.pop("prompt")
        source = repo / "private/prompt.md"
        source.write_text("sensitive")
        task[field] = str(source)
    elif field == "add_dirs":
        task[field] = [str(repo / "private")]
    else:
        task[field] = str(repo / "private")
    result = importlib.import_module("skills.orchestrate.scripts.dispatch_preflight").preflight_tasks([task])
    assert result["status"] == "rejected"
    assert "private" in result["fix"] and "non-training route" in result["fix"]


@pytest.mark.parametrize("field", ["prompt_file", "add_dirs"])
def test_relative_protected_inputs_anchor_to_workspace(monkeypatch, tmp_path, field):
    repo = protected_repo(tmp_path)
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(ROOT))
    (repo / "private/prompt.md").write_text("secret", encoding="utf-8")
    task = {"id": "one", "adapter": "opencode", "model": "opencode/mimo-v2.6-flash-free",
            "prompt": "hello", "cwd": str(repo / "safe")}
    if field == "prompt_file":
        task.pop("prompt")
        task[field] = "private/prompt.md"
    else:
        task[field] = ["private"]
    result = importlib.import_module("skills.orchestrate.scripts.dispatch_preflight").preflight_tasks([task], repo)
    assert result["status"] == "rejected"
    assert "protected path" in result["fix"]


def test_unresolved_training_flag_fails_closed_without_sandbox(monkeypatch, tmp_path):
    mod = supervisor()
    repo = protected_repo(tmp_path)
    force_sandbox(monkeypatch, None)
    with pytest.raises(ValueError, match="sandbox-exec"):
        mod.build_plan("claude", {}, "hello", cwd=repo / "safe", workspace_root=repo)


def test_protected_preflight_rejects_unavailable_sandbox(monkeypatch, tmp_path):
    repo = protected_repo(tmp_path)
    monkeypatch.chdir(repo)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(ROOT))
    preflight = importlib.import_module("skills.orchestrate.scripts.dispatch_preflight")
    force_sandbox(monkeypatch, None)
    task = {"id": "one", "adapter": "opencode", "model": "opencode/mimo-v2.6-flash-free",
            "prompt": "hello", "cwd": str(repo / "safe")}
    result = preflight.preflight_tasks([task])
    assert result["status"] == "rejected"
    assert "sandbox-exec" in result["fix"]


def test_plan_only_uses_explicit_workspace_root_when_process_cwd_differs(monkeypatch, tmp_path, capsys):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    workspace_root = tmp_path / "fabric-workspace"
    workspace_root.mkdir()
    process_cwd = tmp_path / "launcher"
    provider_cwd = workspace_root / "provider"
    process_cwd.mkdir()
    provider_cwd.mkdir()
    route_file = tmp_path / "route.json"
    route_file.write_text('{"resolved_model":"fixture"}', encoding="utf-8")
    prompt_file = tmp_path / "prompt.md"
    prompt_file.write_text("hello", encoding="utf-8")
    output_file = tmp_path / "out.md"
    monkeypatch.chdir(process_cwd)
    monkeypatch.setattr(sys, "argv", [
        "provider_exec.py", "--route-file", str(route_file), "--adapter", "agy",
        "--prompt-file", str(prompt_file), "--out", str(output_file), "--plan-only",
        "--cwd", str(provider_cwd), "--workspace-root", str(workspace_root),
    ])

    assert supervisor.main() == 0
    plan = json.loads(capsys.readouterr().out)
    assert plan["cwd"] == str(provider_cwd.resolve())
    assert plan["workspace_root"] == str(workspace_root.resolve())


def test_main_rejects_cwd_outside_explicit_workspace_root(monkeypatch, tmp_path, capsys):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    launcher = tmp_path / "launcher"
    root = tmp_path / "workspace"
    outside = tmp_path / "outside"
    launcher.mkdir()
    root.mkdir()
    outside.mkdir()
    route_file = tmp_path / "route.json"
    route_file.write_text('{"resolved_model":"fixture"}', encoding="utf-8")
    prompt_file = tmp_path / "prompt.md"
    prompt_file.write_text("hello", encoding="utf-8")
    monkeypatch.chdir(launcher)
    monkeypatch.setattr(sys, "argv", [
        "provider_exec.py", "--route-file", str(route_file), "--adapter", "agy",
        "--prompt-file", str(prompt_file), "--out", str(tmp_path / "out.md"), "--plan-only",
        "--cwd", str(outside), "--workspace-root", str(root),
    ])

    assert supervisor.main() == 2
    assert json.loads(capsys.readouterr().out)["fix"] == "cwd must be inside the workspace"


def test_build_plan_rejects_cwd_outside_explicit_workspace_root(tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / "workspace"
    outside = tmp_path / "outside"
    root.mkdir()
    outside.mkdir()

    with pytest.raises(ValueError, match="cwd must be inside the workspace"):
        supervisor.build_plan(
            "agy", {"resolved_model": "fixture"}, "hello",
            cwd=outside, workspace_root=root,
        )


def test_build_plan_accepts_read_cwd_inside_an_authorised_read_root(tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / "workspace"
    other = tmp_path / "other-project"
    root.mkdir()
    (other / "src").mkdir(parents=True)

    plan = supervisor.build_plan(
        "agy", {"resolved_model": "fixture"}, "hello",
        cwd=other / "src", workspace_root=root, read_roots=[other],
    )

    assert plan["cwd"] == str((other / "src").resolve())
    assert plan["workspace_root"] == str(root.resolve())


def test_build_plan_refuses_a_credential_store_as_read_root(tmp_path, monkeypatch):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    monkeypatch.setenv("HOME", str(tmp_path))
    root = tmp_path / "workspace"
    root.mkdir()
    (tmp_path / ".ssh").mkdir()

    with pytest.raises(ValueError, match="read roots must exclude credential"):
        supervisor.build_plan(
            "agy", {"resolved_model": "fixture"}, "hello",
            cwd=tmp_path / ".ssh", workspace_root=root, read_roots=[tmp_path / ".ssh"],
        )


def test_build_plan_accepts_writer_worktree_outside_the_callers_tree(tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    repo = tmp_path / "repo"
    repo.mkdir()
    git_repo(repo, branch="main")
    caller = tmp_path / "caller"
    sibling = tmp_path / "sibling"
    make_linked_worktree(repo, "caller", path=caller)
    make_linked_worktree(repo, "sibling", path=sibling)

    plan = supervisor.build_plan(
        "codex", {"resolved_model": "fixture"}, "hello",
        workspace_root=caller, mode="worktree_write", worktree=sibling,
    )

    assert plan["cwd"] == str(sibling.resolve())


def test_build_plan_defaults_cwd_to_the_workspace_root(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / "workspace"
    elsewhere = tmp_path / "elsewhere"
    root.mkdir()
    elsewhere.mkdir()
    monkeypatch.chdir(elsewhere)

    plan = supervisor.build_plan("agy", {"resolved_model": "fixture"}, "hello", workspace_root=root)

    assert plan["cwd"] == str(root.resolve())


@pytest.mark.parametrize("adapter", ["agy", "claude"])
def test_read_only_profile_denies_writes_outside_attempt_and_provider_state(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    home = tmp_path / "home"
    workspace = home / "repo"
    attempt = workspace / ".agent-run/attempt"
    add_dir = tmp_path / "extra"
    attempt.mkdir(parents=True)
    add_dir.mkdir()
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    monkeypatch.setattr(mod.provider_confinement, "CONFINED_STATE", {adapter: {
        "home": ".claude", "read_write": ("state",), "read": (),
    }})
    plan = {"adapter": adapter, "mode": "read_only", "workspace_root": str(workspace),
            "cwd": str(workspace), "run_dir": str(attempt),
            "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]}}
    profile = mod.provider_confinement.os_confinement_profile(plan)
    assert "(deny file-write*)" in profile
    allow = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-write* "))
    assert f'(subpath "{attempt}")' in allow
    assert f'(subpath "{home / "state"}")' in allow
    assert '(subpath "/dev")' in allow
    for path in (workspace, add_dir, home, tmp_path / "T", Path("/private/tmp")):
        assert f'(subpath "{path}")' not in allow


@pytest.mark.parametrize(
    "route,api_key,granted",
    [
        ({}, None, True),
        ({"endpoint_base_url": "https://api.example.invalid/anthropic"}, None, False),
        ({}, "sk-test", False),
    ],
    ids=["oauth", "endpoint", "api-key"],
)
def test_claude_read_only_profile_reads_login_keychain_only_for_oauth_lanes(
    monkeypatch, tmp_path, route, api_key, granted
):
    mod = supervisor()
    home = tmp_path / "home"
    workspace = home / "repo"
    workspace.mkdir(parents=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    if api_key is None:
        monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    else:
        monkeypatch.setenv("ANTHROPIC_API_KEY", api_key)
    plan = {"adapter": "claude", "mode": "read_only", "route": route, "workspace_root": str(workspace),
            "cwd": str(workspace), "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    profile = mod.provider_confinement.os_confinement_profile(plan)
    allow = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-read-data "))
    # An OAuth lane reads its sign-in from the login keychain; a bare or endpoint lane uses a key.
    assert (f'"{home / "Library/Keychains/login.keychain-db"}"' in allow) is granted
    assert f'(subpath "{home / "Library/Keychains"}")' not in allow


def test_read_only_profile_reads_the_adapter_binary_through_its_launcher_link_and_install_folder(monkeypatch, tmp_path):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    workspace = home / "repo"
    install = home / ".local/share/cursor-agent/versions/1.0-abc"
    other = home / ".local/share/cursor-agent/versions/0.9-old"
    for directory in (workspace, install, other, home / ".local/bin", home / ".ssh"):
        directory.mkdir(parents=True)
    (install / "cursor-agent").write_text("#!/bin/sh\nexec cat \"$(dirname \"$(readlink \"$0\")\")/index.js\"\n", encoding="utf-8")
    (install / "cursor-agent").chmod(0o755)
    (install / "index.js").write_text("ok\n", encoding="utf-8")
    (other / "index.js").write_text("old\n", encoding="utf-8")
    (home / ".ssh/id").write_text("secret\n", encoding="utf-8")
    (home / ".local/bin/cursor-agent").symlink_to(install / "cursor-agent")
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    plan = {"adapter": "cursor", "mode": "read_only", "workspace_root": str(workspace), "cwd": str(workspace),
            "argv": ["cursor-agent"], "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    search_path = os.pathsep.join([str(home / ".local/bin"), "/usr/bin", "/bin"])
    profile = mod.provider_confinement.os_confinement_profile(plan, search_path)
    reads = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-read-data "))
    assert f'(literal "{home}/.local/bin/cursor-agent")' in reads
    assert f'(subpath "{install}")' in reads
    # cursor-agent signs in through the login keychain.
    assert f'(subpath "{home}/Library/Keychains/login.keychain-db")' in reads
    # The link's folder, its sibling versions and the rest of home stay closed.
    assert f'(subpath "{home / ".local/bin"}")' not in reads
    assert f'(subpath "{other}")' not in reads
    assert f'(subpath "{home / ".local/share/cursor-agent"}")' not in reads
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if sys.platform != "darwin" or not sandbox_exec:
        return
    launched = subprocess.run([sandbox_exec, "-p", profile, str(home / ".local/bin/cursor-agent")],
                              capture_output=True, text=True)
    if launched.returncode and "sandbox_apply" in launched.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert launched.returncode == 0, launched.stderr
    assert launched.stdout == "ok\n"
    for denied in (other / "index.js", home / ".ssh/id"):
        refused = subprocess.run([sandbox_exec, "-p", profile, "/bin/cat", str(denied)],
                                 capture_output=True, text=True)
        assert refused.returncode != 0, denied


def test_adapter_binary_in_a_plain_bin_directory_grants_the_file_alone(monkeypatch, tmp_path):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    (home / ".local/bin").mkdir(parents=True)
    tool = home / ".local/bin/tool"
    tool.write_text("#!/bin/sh\n", encoding="utf-8")
    tool.chmod(0o755)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    links, directories = mod.provider_confinement._adapter_binary_reads({"argv": ["tool"]}, str(home / ".local/bin"))
    assert links == [tool]
    assert directories == []


PROVIDER_STATE_CASES = (
    ("claude", (".claude/settings.json", ".claude/settings.local.json", ".claude/hooks/attack.sh",
                ".claude/skills/attack/SKILL.md", ".claude/agents/attack.md", ".claude/plugins/attack.js",
                ".claude/CLAUDE.md", ".claude/shell-snapshots/attack.sh",
                ".claude/projects/probe/memory/MEMORY.md", ".claude/projects/probe/settings.json",
                ".claude.json", ".claude.json.tmp", ".claude.json.backup"),
     (".claude/projects/probe/12345678-1234-1234-1234-123456789abc.jsonl",
      ".claude/projects/probe/12345678-1234-1234-1234-123456789abc/subagents/agent.jsonl",
      ".claude/sessions/probe.json", ".claude/history.jsonl",
      ".claude/.credentials.json", ".claude/.credentials.json.tmp", ".claude/debug/probe.log")),
    ("agy", (".gemini/config/mcp_config.json", ".gemini/config/plugins/attack.js",
             ".gemini/config/skills/attack/SKILL.md", ".gemini/antigravity-cli/settings.json",
             ".gemini/antigravity-cli/plugins/attack.js", ".gemini/antigravity-cli/skills/attack/SKILL.md",
             ".gemini/antigravity-cli/bin/attack", ".gemini/antigravity-cli/bin/agentapi",
             ".gemini/antigravity-cli/builtin/skills/attack.md",
             ".gemini/antigravity-cli/mcp/server/instructions.md",
             ".gemini/antigravity-cli/cache/marketplaces/attack.js", ".gemini/config/projects/probe.json",
             ".gemini/config/sidecars/attack.json"),
     (".gemini/oauth_creds.json", ".gemini/oauth_creds.json.tmp",
      ".gemini/antigravity-cli/antigravity-oauth-token", ".gemini/antigravity-cli/mcp_oauth_tokens.json",
      ".gemini/antigravity-cli/conversations/probe.pb", ".gemini/antigravity-cli/presence/probe.lock",
      ".gemini/antigravity-cli/conversation_summaries.db-wal")),
    ("cursor", (".cursor/hooks.json", ".cursor/extensions/attack.js", ".cursor/settings.json",
                ".cursor/cli-config.json", ".cursor/argv.json", ".cursor/mcp.json",
                ".cursor/agents/attack.md", ".cursor/plugins/attack.js", ".cursor/skills/attack/SKILL.md",
                ".cursor/projects/probe/.workspace-trusted", ".cursor/projects/probe/repo.json",
                ".cursor/projects/probe/mcp-approvals.json", ".cursor/projects/probe/mcp-disabled.json",
                ".cursor/projects/probe/mcps/server/instructions.md", ".cursor/projects/probe/rules/attack.md"),
     (".cursor/chats/probe/session.db", ".cursor/agent-cli-state.json", ".cursor/prompt_history.json",
      ".cursor/ai-tracking/ai-code-tracking.db-wal", ".cursor/projects/probe/agent-transcripts/probe.txt",
      ".cursor/projects/probe/worker.log", ".cursor/projects/probe/mcp-auth.json")),
    ("kiro", (".kiro/settings/mcp.json", ".kiro/settings/cli.json", ".kiro/agents/attack.json",
              ".kiro/steering/attack.md"),
     (".cache/kiro/probe.log",)),
)


@pytest.mark.parametrize("mode", ["read_only", "worktree_write"])
@pytest.mark.parametrize("adapter,protected,writable", PROVIDER_STATE_CASES)
@pytest.mark.parametrize("sandboxed", [False, True], ids=["profile", "sandbox-exec"])
def test_provider_state_keeps_config_read_only_and_runtime_state_writable(monkeypatch, tmp_path, mode,
                                                                        adapter, protected, writable, sandboxed):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    workspace = home / "repo"
    workspace.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    for relative in (*protected, *writable):
        path = home / relative
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("original\n", encoding="utf-8")
    profile = mod.provider_confinement.os_confinement_profile({
        "adapter": adapter, "mode": mode, "workspace_root": str(workspace), "cwd": str(workspace),
        "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    })
    assert "(deny file-write*)" in profile
    writes = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-write* "))
    # Check every allow, including ancestor grants and atomic-rewrite regexes, not just exact names.
    def granted(relative):
        path = str(home / relative)
        for kind, value in re.findall(r'\((subpath|literal|regex) #?"([^"]*)"\)', writes):
            if kind == "regex" and re.search(value, path):
                return True
            if kind == "literal" and path == value:
                return True
            if kind == "subpath" and (path == value or path.startswith(value + "/")):
                return True
        return False

    for relative in protected:
        assert not granted(relative), relative
    for relative in writable:
        assert granted(relative), relative
    if adapter in {"claude", "cursor"}:
        assert not granted(f".{adapter}/projects/absent-project")
    root = {"agy": ".gemini", "claude": ".claude", "cursor": ".cursor", "kiro": ".kiro"}[adapter]
    link_guard = f'(deny file-write-create (require-all (subpath "{home / root}")'
    assert any(line.startswith(link_guard) and "(vnode-type SYMLINK)" in line for line in profile.splitlines())

    if not sandboxed:
        return
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")

    def run(script):
        return subprocess.run([sandbox_exec, "-p", profile, "/bin/sh", "-c", script],
                              capture_output=True, text=True)

    probe = run("true")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert probe.returncode == 0, probe.stderr
    for relative in protected:
        path = shlex.quote(str(home / relative))
        assert run(f"printf hijack > {path}").returncode != 0, relative
        assert run(f"rm -f {path}").returncode != 0, relative
        assert (home / relative).read_text() == "original\n"
    for relative in writable:
        path = shlex.quote(str(home / relative))
        assert run(f"printf refreshed > {path}").returncode == 0, relative
        link = shlex.quote(str(home / (relative + ".link")))
        assert run(f"ln -s {shlex.quote(str(home / protected[0]))} {link}").returncode != 0, relative
    # New session directories must still be creatable; the file-only Kiro guard would forbid these.
    sessions = {"claude": ".claude/projects/probe/12345678-1234-1234-1234-123456789abc",
                "agy": ".gemini/antigravity-cli/conversations", "cursor": ".cursor/chats/probe",
                "kiro": ".cache/kiro"}
    directory = home / sessions[adapter] / "new-session"
    assert run(f"mkdir {shlex.quote(str(directory))}").returncode == 0
    link = directory / "link"
    assert run(f"ln -s {shlex.quote(str(home / protected[0]))} {shlex.quote(str(link))}").returncode != 0
    if adapter in {"claude", "cursor"}:
        destination = home / f".{adapter}/projects/absent-project"
        assert run(f"mv {shlex.quote(str(directory))} {shlex.quote(str(destination))}").returncode != 0


@pytest.mark.parametrize("mode", ["read_only", "worktree_write"])
@pytest.mark.parametrize("adapter,config", [
    ("claude", ".claude/settings.json"), ("agy", ".gemini/config/mcp_config.json"),
    ("cursor", ".cursor/cli-config.json"), ("kiro", ".kiro/settings/mcp.json"),
    ("opencode", ".config/opencode/opencode.json"), ("claude", ".claude.json"),
    ("claude", ".ssh/synthetic"),
])
def test_file_link_cannot_cross_protected_provider_state(monkeypatch, tmp_path,
                                                                         adapter, config, mode):
    mod = supervisor()
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    home = (tmp_path / "home").resolve()
    workspace = home / "repo"
    workspace.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    protected = home / config
    protected.parent.mkdir(parents=True, exist_ok=True)
    protected.write_text("original\n")
    attempt = home / "attempt"
    attempt.mkdir()
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    plan = {"adapter": adapter, "mode": mode, "cwd": str(workspace), "workspace_root": str(workspace),
            "run_dir": str(attempt), "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    command = mod.provider_confinement.confinement_command(plan, ["/bin/sh", "-c"])
    probe = subprocess.run([*command, f"printf ok > {shlex.quote(str(attempt / 'probe'))}"],
                           check=False, capture_output=True, text=True)
    assert probe.returncode == 0, probe.stderr
    # Write rules check both link ends, so links inside writable roots (local git clones) still work.
    inside = attempt / "probe-link"
    result = subprocess.run([*command, f"ln {shlex.quote(str(attempt / 'probe'))} {shlex.quote(str(inside))}"],
                            check=False, capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    link = attempt / "x"
    result = subprocess.run([*command, f"ln {shlex.quote(str(protected))} {shlex.quote(str(link))}"],
                            check=False, capture_output=True, text=True)
    assert result.returncode != 0, result.stderr
    assert not link.exists()
    destination = protected.parent / "new-link"
    result = subprocess.run([*command, f"ln {shlex.quote(str(attempt / 'probe'))} {shlex.quote(str(destination))}"],
                            check=False, capture_output=True, text=True)
    assert result.returncode != 0, result.stderr
    assert not destination.exists()
    assert protected.read_text() == "original\n"


@pytest.mark.parametrize("sandboxed", [False, True], ids=["preparation", "sandbox-exec"])
@pytest.mark.parametrize("adapter", ["claude", "cursor", "agy", "kiro"])
def test_provider_state_symlink_home_and_cwd_alias_use_canonical_paths(monkeypatch, tmp_path, adapter, sandboxed):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    home.mkdir()
    target = (tmp_path / "dotfiles" / adapter).resolve()
    target.mkdir(parents=True)
    relative_home = ".gemini" if adapter == "agy" else f".{adapter}"
    (home / relative_home).symlink_to(target, target_is_directory=True)
    workspace = (tmp_path / "real-project").resolve()
    workspace.mkdir()
    alias = tmp_path / "project-alias"
    alias.symlink_to(workspace, target_is_directory=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    sandbox_exec = mod.provider_confinement._sandbox_exec_path() if sandboxed else None
    if sandboxed and not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    # Preparation and canonical paths can be checked even in a nested sandbox.
    force_sandbox(monkeypatch, sandbox_exec or "/usr/bin/sandbox-exec")
    plan = {"adapter": adapter, "mode": "read_only", "cwd": str(alias), "workspace_root": str(workspace),
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    command = mod.provider_confinement.confinement_command(plan, ["/bin/sh", "-c"])
    slug = (mod.provider_confinement._claude_project_dir(home, workspace).name if adapter == "claude" else
            re.sub(r"[^A-Za-z0-9]+", "-", str(workspace)).strip("-"))
    project = (target if adapter == "kiro" else target / "antigravity-cli/scratch" if adapter == "agy" else
               target / "projects" / slug)
    assert project.is_dir()
    # The exact same canonical root anchors the regex, not just directory preparation.
    patterns = re.findall(r'\(allow file-write\* \(regex #"([^"]+)"\)\)', command[2])
    if adapter == "kiro":
        assert f'(allow file-read-data (subpath "{target}")' in command[2]
        assert f'(subpath "{home / ".kiro"}")' not in command[2]
        if sandbox_exec:
            (target / "synthetic").write_text("ok")
            result = subprocess.run([*command, f"cat {shlex.quote(str(target / 'synthetic'))}"],
                                    check=False, capture_output=True, text=True)
            assert result.returncode == 0, result.stderr
            assert result.stdout == "ok"
            assert subprocess.run([*command, f"printf bad > {shlex.quote(str(target / 'settings.json'))}"],
                                  check=False, capture_output=True, text=True).returncode != 0
        return
    state = (project / "12345678-1234-1234-1234-123456789abc.jsonl" if adapter == "claude" else
             project / "worker.log")
    if adapter == "agy":
        assert f'(allow file-write* (subpath "{target / "antigravity-cli/annotations"}")' in command[2]
    else:
        assert any(re.fullmatch(pattern, str(state)) for pattern in patterns)
    if not sandbox_exec:
        return
    result = subprocess.run([*command, f"printf ok > {shlex.quote(str(state))}"], check=False, capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    assert state.read_text() == "ok"
    denied = target / {"claude": "settings.json", "cursor": "cli-config.json", "agy": "config/mcp_config.json"}[adapter]
    denied.parent.mkdir(parents=True, exist_ok=True)
    assert subprocess.run([*command, f"printf bad > {shlex.quote(str(denied))}"],
                          check=False, capture_output=True, text=True).returncode != 0


@pytest.mark.parametrize("sandboxed", [False, True], ids=["preparation", "sandbox-exec"])
@pytest.mark.parametrize("relative", ["cache/projects.json", "updater/update_status.json"])
def test_agy_prepares_fresh_state_and_allows_atomic_runtime_rewrites(monkeypatch, tmp_path, relative, sandboxed):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    home.mkdir()
    workspace = tmp_path / "repo"
    workspace.mkdir()
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    sandbox_exec = mod.provider_confinement._sandbox_exec_path() if sandboxed else None
    if sandboxed and not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    force_sandbox(monkeypatch, sandbox_exec or "/usr/bin/sandbox-exec")
    plan = {"adapter": "agy", "mode": "read_only", "cwd": str(workspace), "workspace_root": str(workspace),
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    command = mod.provider_confinement.confinement_command(plan, ["/bin/sh", "-c"])
    root = home / ".gemini/antigravity-cli"
    for name in ("updater", "scratch", "cache", "conversations", "presence"):
        assert (root / name).is_dir(), name
    path = root / relative
    siblings = re.findall(r'\(allow file-write\* .*', command[2])
    assert any(re.search(pattern, str(path) + ".tmp") for rule in siblings
               for pattern in re.findall(r'\(regex #"([^"]+)"\)', rule))
    if not sandbox_exec:
        return
    result = subprocess.run([*command, f"printf new > {shlex.quote(str(path) + '.tmp')} && "
                                      f"mv {shlex.quote(str(path) + '.tmp')} {shlex.quote(str(path))}"],
                            capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    assert path.read_text() == "new"
    result = subprocess.run([*command, f"mkdir {shlex.quote(str(root / 'scratch/new-session'))}"],
                            capture_output=True, text=True)
    assert result.returncode == 0, result.stderr
    for denied in (root / "cache/marketplaces/attack.json", home / ".gemini/config/sidecars/attack.json"):
        denied.parent.mkdir(parents=True, exist_ok=True)
        assert subprocess.run([*command, f"printf bad > {shlex.quote(str(denied))}"],
                              capture_output=True, text=True).returncode != 0


AGY_TOKENS = (".gemini/oauth_creds.json", ".gemini/antigravity-cli/antigravity-oauth-token",
              ".gemini/antigravity-cli/mcp_oauth_tokens.json")


def agy_home_fixture(mod, monkeypatch, tmp_path):
    home = (tmp_path / "home").resolve()
    root = home / ".gemini/antigravity-cli"
    for relative in (".gitconfig", ".gemini/config/settings.json", *AGY_TOKENS,
                     ".gemini/antigravity-cli/settings.json", ".gemini/antigravity-cli/conversations/c.pb",
                     ".gemini/antigravity-cli/jetski_state.pbtxt", ".gemini/antigravity-cli/history.jsonl",
                     ".gemini/antigravity-cli/bin/agentapi", ".gemini/antigravity-cli/bin/webm_encoder"):
        (home / relative).parent.mkdir(parents=True, exist_ok=True)
        (home / relative).write_text("original\n", encoding="utf-8")
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    return home, root


def stage_agy(mod, plan, private_tmp):
    """Stage as execute does, in the attempt tmp opened once without following a link."""
    tmp_fd = mod.provider_confinement._open_canonical_dir(private_tmp)
    return mod.agy_home.stage_agy_home(plan, tmp_fd, private_tmp), tmp_fd


def agy_plan(**extra):
    return {"adapter": "agy", "mode": "read_only", "applied": {"confinement": "sandbox-exec", "add_dirs": []},
            **extra}


def test_confined_agy_home_links_everything_but_its_bin_shim_and_token_files(monkeypatch, tmp_path):
    mod = supervisor()
    home, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    private_tmp = tmp_path / "attempt/tmp"
    private_tmp.mkdir(parents=True)
    plan = agy_plan()
    overlay = stage_agy(mod, plan, private_tmp)[0].path
    assert overlay.parent == private_tmp and overlay.name.startswith("agy-home-")
    lane_root = overlay / ".gemini/antigravity-cli"
    for directory in (overlay, overlay / ".gemini", lane_root, lane_root / "bin"):
        assert directory.is_dir() and not directory.is_symlink(), directory
    # State directories are prepared before staging, so each is a link and nothing is lost at cleanup.
    for relative in (".gitconfig", ".gemini/config", ".gemini/antigravity-cli/settings.json",
                     ".gemini/antigravity-cli/conversations", ".gemini/antigravity-cli/brain",
                     ".gemini/antigravity-cli/updater", ".gemini/antigravity-cli/bin/webm_encoder"):
        assert (overlay / relative).readlink() == home / relative, relative
    for relative in (*AGY_TOKENS, ".gemini/antigravity-cli/bin/agentapi"):
        assert not (overlay / relative).exists() and not (overlay / relative).is_symlink(), relative

    # Each attempt stages a fresh overlay, whatever a lane or a failed cleanup left in the last one.
    (lane_root / "bin/agentapi").write_text("planted\n", encoding="utf-8")
    (lane_root / "settings.json").unlink()
    (lane_root / "settings.json").write_text("planted\n", encoding="utf-8")
    os.mkfifo(private_tmp / "agy-home")
    again = stage_agy(mod, plan, private_tmp)[0].path
    assert again != overlay
    assert not (again / ".gemini/antigravity-cli/bin/agentapi").exists()
    assert (again / ".gemini/antigravity-cli/settings.json").readlink() == root / "settings.json"
    assert (root / "bin/agentapi").read_text() == "original\n"

    assert stage_agy(mod, {**plan, "adapter": "claude"}, private_tmp)[0] is None
    assert stage_agy(mod, agy_plan(applied={"confinement": "none"}), private_tmp)[0] is None


def test_agy_home_keeps_dangling_links_and_tolerates_a_missing_bin(monkeypatch, tmp_path):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    home.mkdir()
    (home / ".dangling").symlink_to(home / "absent")
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    private_tmp = tmp_path / "tmp"
    private_tmp.mkdir()
    overlay = stage_agy(mod, agy_plan(), private_tmp)[0].path
    assert (overlay / ".dangling").readlink() == home / ".dangling"
    assert not (home / ".gemini/antigravity-cli/bin").exists()
    assert list((overlay / ".gemini/antigravity-cli/bin").iterdir()) == []
    assert (overlay / ".gemini/antigravity-cli/conversations").readlink() == home / ".gemini/antigravity-cli/conversations"


def test_agy_home_copies_back_renamed_state_but_never_tokens_links_or_oversized_files(monkeypatch, tmp_path):
    mod = supervisor()
    agy_state = mod.provider_confinement.CONFINED_STATE["agy"]
    granted = {entry.removesuffix("*") for entry in (*agy_state["read_write"], *agy_state["write_literal"])}
    # Only state the profile already lets the lane write is ever copied back.
    assert {f".gemini/antigravity-cli/{name}" for name in mod.agy_home.AGY_RENAMED_STATE} <= granted
    _, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    (root / "jetski_state.pbtxt").chmod(0o644)
    private_tmp = tmp_path / "tmp"
    private_tmp.mkdir()
    outside = tmp_path / "outside"
    outside.write_text("outside\n", encoding="utf-8")
    plan = agy_plan()
    staged, tmp_fd = stage_agy(mod, plan, private_tmp)
    overlay = staged.path
    lane_root = overlay / ".gemini/antigravity-cli"
    # agy writes a temp file and renames it over the overlay's link.
    for name, text in (("jetski_state.pbtxt", "renamed\n"), ("antigravity-oauth-token", "token\n")):
        (lane_root / (name + ".tmp")).write_text(text, encoding="utf-8")
        os.replace(lane_root / (name + ".tmp"), lane_root / name)
    (lane_root / "history.jsonl").unlink()
    (lane_root / "history.jsonl").symlink_to(outside)  # a link is never followed or installed
    os.mkfifo(lane_root / "last_check.timestamp")  # nor a FIFO, which must not block the copy
    (lane_root / "installation_id").write_text("x" * 65, encoding="utf-8")
    monkeypatch.setattr(mod.agy_home, "AGY_STATE_COPY_MAX", 64)
    (lane_root / "locked").mkdir()
    (lane_root / "locked/file").write_text("x", encoding="utf-8")
    (lane_root / "locked").chmod(0o500)

    warnings = mod.agy_home.finish_agy_home(plan, tmp_fd, staged)
    assert (root / "jetski_state.pbtxt").read_text() == "renamed\n"
    assert stat.S_IMODE((root / "jetski_state.pbtxt").stat().st_mode) == 0o644
    assert not (root / "jetski_state.pbtxt").is_symlink()
    assert (root / "antigravity-oauth-token").read_text() == "original\n"
    assert (root / "history.jsonl").read_text() == "original\n" and outside.read_text() == "outside\n"
    assert not (root / "last_check.timestamp").exists()
    assert not (root / "installation_id").exists()
    assert sorted(warnings) == ["agy state not copied back: installation_id: larger than 64 bytes",
                                "agy state not copied back: last_check.timestamp: not a regular file"]
    assert not overlay.exists()  # read-only directories and FIFOs included
    assert sorted(path.name for path in root.iterdir() if path.name.startswith(".")) == [mod.agy_home.AGY_COPY_BACK_LOCK]


def test_confined_agy_writes_its_shim_in_the_overlay_and_never_the_live_bin(monkeypatch, tmp_path):
    """Verified on a Mac only: CI runs Ubuntu, which has no sandbox-exec, so this skips there."""
    mod = supervisor()
    _, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    workspace = tmp_path / "repo"
    workspace.mkdir()
    run_dir = tmp_path / "attempt"
    (run_dir / "tmp").mkdir(parents=True)
    plan = agy_plan(cwd=str(workspace), workspace_root=str(workspace), run_dir=str(run_dir))
    profile = mod.provider_confinement.os_confinement_profile(plan)
    overlay = stage_agy(mod, plan, run_dir / "tmp")[0].path

    def run(script):
        return subprocess.run([sandbox_exec, "-p", profile, "/bin/sh", "-c", script],
                              capture_output=True, text=True, check=False,
                              env={**os.environ, "HOME": str(overlay)})

    probe = run("true")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    shim = '#!/bin/sh\nexec "/usr/local/bin/agy" agentapi "$@"\n'
    assert run(f'printf {shlex.quote(shim)} > "$HOME/.gemini/antigravity-cli/bin/agentapi"').returncode == 0
    assert (overlay / ".gemini/antigravity-cli/bin/agentapi").read_text() == shim
    # Every link is judged at its real path: state stays writable, config and the live bin do not.
    assert run('printf new > "$HOME/.gemini/antigravity-cli/conversations/c.pb"').returncode == 0
    assert (root / "conversations/c.pb").read_text() == "new"
    for relative in ("settings.json", "bin/webm_encoder"):
        assert run(f'printf bad > "$HOME/.gemini/antigravity-cli/{relative}"').returncode != 0, relative
        assert (root / relative).read_text() == "original\n"
    assert run(f"printf bad > {shlex.quote(str(root / 'bin/agentapi'))}").returncode != 0
    assert (root / "bin/agentapi").read_text() == "original\n"


def test_attempt_tmp_replaces_a_planted_link_and_refuses_a_linked_attempt(monkeypatch, tmp_path):
    mod = supervisor()
    agy_home_fixture(mod, monkeypatch, tmp_path)
    victim = tmp_path / "victim/tmp"
    victim.mkdir(parents=True)
    attempt = tmp_path / "attempt"
    attempt.mkdir()
    (attempt / "tmp").symlink_to(victim)
    tmp_fd = mod.provider_confinement._open_attempt_tmp(attempt)
    staged = mod.agy_home.stage_agy_home(agy_plan(), tmp_fd, attempt / "tmp")
    assert (attempt / "tmp").is_dir() and not (attempt / "tmp").is_symlink()
    assert stat.S_IMODE((attempt / "tmp").stat().st_mode) == 0o700
    assert staged.path.is_dir() and list(victim.iterdir()) == []
    os.close(tmp_fd)
    linked = tmp_path / "linked-attempt"
    linked.symlink_to(attempt)
    with pytest.raises(OSError):
        mod.provider_confinement._open_attempt_tmp(linked)


def _plant_victim_overlay(victim, name):
    lane_root = victim / name / ".gemini/antigravity-cli"
    lane_root.mkdir(parents=True)
    (lane_root / "jetski_state.pbtxt").write_text("victim\n", encoding="utf-8")
    return lane_root


def _rename_into(lane_root, name, text):
    (lane_root / (name + ".tmp")).write_text(text, encoding="utf-8")
    os.replace(lane_root / (name + ".tmp"), lane_root / name)


def test_agy_home_cleanup_never_follows_a_tmp_or_overlay_swapped_for_a_link(monkeypatch, tmp_path):
    """A lane that swaps tmp for another attempt's tmp cannot redirect copy-back or removal."""
    mod = supervisor()
    _, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    attempt = tmp_path / "attempt"
    (attempt / "tmp").mkdir(parents=True)
    plan = agy_plan()
    staged, tmp_fd = stage_agy(mod, plan, attempt / "tmp")
    _rename_into(staged.path / ".gemini/antigravity-cli", "jetski_state.pbtxt", "lane\n")
    victim = tmp_path / "other-attempt/tmp"
    victim_root = _plant_victim_overlay(victim, staged.name)
    (attempt / "tmp").rename(attempt / "moved")
    (attempt / "tmp").symlink_to(victim)

    assert mod.agy_home.finish_agy_home(plan, tmp_fd, staged) == []
    assert (root / "jetski_state.pbtxt").read_text() == "lane\n"
    assert (victim_root / "jetski_state.pbtxt").read_text() == "victim\n"
    assert list((attempt / "moved").iterdir()) == []

    # The same holds for a link planted inside the overlay itself.
    staged = mod.agy_home.stage_agy_home(plan, tmp_fd, attempt / "moved")
    victim_root = _plant_victim_overlay(victim, "planted")
    shutil.rmtree(staged.path / ".gemini")
    (staged.path / ".gemini").symlink_to(victim / "planted/.gemini")
    warnings = mod.agy_home.finish_agy_home(plan, tmp_fd, staged)
    assert len(warnings) == 1 and warnings[0].startswith("agy state not copied back: "), warnings
    assert (root / "jetski_state.pbtxt").read_text() == "lane\n"
    assert (victim_root / "jetski_state.pbtxt").read_text() == "victim\n"
    assert list((attempt / "moved").iterdir()) == []


@pytest.mark.parametrize("swapped", [".gemini", ".gemini/antigravity-cli"])
def test_agy_home_copy_back_never_follows_a_real_state_parent_swapped_for_a_link(monkeypatch, tmp_path, swapped):
    mod = supervisor()
    home, _ = agy_home_fixture(mod, monkeypatch, tmp_path)
    private_tmp = tmp_path / "tmp"
    private_tmp.mkdir()
    plan = agy_plan()
    staged, tmp_fd = stage_agy(mod, plan, private_tmp)
    _rename_into(staged.path / ".gemini/antigravity-cli", "jetski_state.pbtxt", "lane\n")
    elsewhere = tmp_path / "elsewhere"
    (elsewhere / "antigravity-cli").mkdir(parents=True)
    (home / swapped).rename(tmp_path / "moved")
    (home / swapped).symlink_to(elsewhere if swapped == ".gemini" else elsewhere / "antigravity-cli")

    warnings = mod.agy_home.finish_agy_home(plan, tmp_fd, staged)
    assert len(warnings) == 1 and warnings[0].startswith("agy state not copied back: "), warnings
    assert list((elsewhere / "antigravity-cli").iterdir()) == []
    assert list(private_tmp.iterdir()) == []


def test_agy_copy_back_keeps_a_real_file_changed_since_staging_and_appends_history(monkeypatch, tmp_path):
    mod = supervisor()
    _, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    (root / "history.jsonl").write_text('{"n": 1}\n', encoding="utf-8")
    private_tmp = tmp_path / "tmp"
    private_tmp.mkdir()
    plan = agy_plan()
    staged, tmp_fd = stage_agy(mod, plan, private_tmp)
    lane_root = staged.path / ".gemini/antigravity-cli"
    _rename_into(lane_root, "jetski_state.pbtxt", "lane\n")
    _rename_into(lane_root, "history.jsonl", '{"n": 1}\n{"lane": 1}\n')
    _rename_into(lane_root, "installation_id", "lane\n")
    # Another session finishes first: it rewrites jetski_state and appends to history.
    (root / "jetski_state.pbtxt.new").write_text("other\n", encoding="utf-8")
    os.replace(root / "jetski_state.pbtxt.new", root / "jetski_state.pbtxt")
    with (root / "history.jsonl").open("a", encoding="utf-8") as history:
        history.write('{"other": 1}\n')

    warnings = mod.agy_home.finish_agy_home(plan, tmp_fd, staged)
    kept = "jetski_state.pbtxt: changed by another session since staging; kept it"
    assert warnings == ["agy state not copied back: " + kept]
    assert (root / "jetski_state.pbtxt").read_text() == "other\n"
    assert (root / "history.jsonl").read_text() == '{"n": 1}\n{"other": 1}\n{"lane": 1}\n'
    assert (root / "installation_id").read_text() == "lane\n"  # absent at staging and still absent

    # A lane that truncated or rewrote history adds nothing.
    staged = mod.agy_home.stage_agy_home(plan, tmp_fd, private_tmp)
    _rename_into(staged.path / ".gemini/antigravity-cli", "history.jsonl", '{"forged": 1}\n')
    assert mod.agy_home.finish_agy_home(plan, tmp_fd, staged) == [
        "agy state not copied back: history.jsonl: shorter than at staging"]
    assert (root / "history.jsonl").read_text() == '{"n": 1}\n{"other": 1}\n{"lane": 1}\n'


def test_agy_history_copy_back_never_reads_a_sparse_staged_size(monkeypatch, tmp_path):
    """A first lane enlarges the real history sparsely before a retry stages; the copy stays bounded."""
    mod = supervisor()
    _, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    with (root / "history.jsonl").open("r+b") as history:
        history.truncate(1 << 30)
    private_tmp = tmp_path / "tmp"
    private_tmp.mkdir()
    plan = agy_plan()
    staged, tmp_fd = stage_agy(mod, plan, private_tmp)
    _rename_into(staged.path / ".gemini/antigravity-cli", "history.jsonl", '{"lane": 1}\n')
    real_pread, requested = os.pread, []

    def bounded(fd, size, offset):
        requested.append(size)
        assert size <= 1024 * 1024, size
        return real_pread(fd, size, offset)

    monkeypatch.setattr(os, "pread", bounded)
    warnings = mod.agy_home.finish_agy_home(plan, tmp_fd, staged)
    monkeypatch.undo()
    assert warnings == ["agy state not copied back: history.jsonl: shorter than at staging"], warnings
    assert (root / "history.jsonl").stat().st_size == 1 << 30


def test_agy_copy_back_refuses_hardlinked_sources_and_history_targets(monkeypatch, tmp_path):
    mod = supervisor()
    _, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    (root / "history.jsonl").write_text('{"n": 1}\n', encoding="utf-8")
    alias = tmp_path / "alias.jsonl"
    os.link(root / "history.jsonl", alias)  # writing through the real name would also change this
    secret = tmp_path / "secret"
    secret.write_text("secret\n", encoding="utf-8")
    private_tmp = tmp_path / "tmp"
    private_tmp.mkdir()
    plan = agy_plan()
    staged, tmp_fd = stage_agy(mod, plan, private_tmp)
    lane_root = staged.path / ".gemini/antigravity-cli"
    (lane_root / "jetski_state.pbtxt").unlink()
    os.link(secret, lane_root / "jetski_state.pbtxt")
    _rename_into(lane_root, "history.jsonl", '{"n": 1}\n{"lane": 1}\n')

    warnings = mod.agy_home.finish_agy_home(plan, tmp_fd, staged)
    assert sorted(warnings) == ["agy state not copied back: history.jsonl: not a single-link regular file",
                                "agy state not copied back: jetski_state.pbtxt: not a single-link regular file"]
    assert (root / "jetski_state.pbtxt").read_text() == "original\n"
    assert alias.read_text() == '{"n": 1}\n' and secret.read_text() == "secret\n"


def test_overlapping_agy_finishers_never_both_install(monkeypatch, tmp_path):
    """Two attempts staged from the same real state finish at once: the first wins, the second warns."""
    import threading
    mod = supervisor()
    _, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    plan = agy_plan()
    finishers = []
    for name in ("first", "second"):
        private_tmp = tmp_path / name
        private_tmp.mkdir()
        staged, tmp_fd = stage_agy(mod, plan, private_tmp)
        _rename_into(staged.path / ".gemini/antigravity-cli", "jetski_state.pbtxt", name + "\n")
        finishers.append((staged, tmp_fd))
    results = {}
    second = threading.Thread(target=lambda: results.update(second=mod.agy_home.finish_agy_home(plan, *reversed(finishers[1]))))
    real_install = mod.agy_home._install_state

    def install_while_the_second_finishes(directory, name, data):
        if not second.is_alive() and "second" not in results:
            second.start()
            second.join(0.5)  # without a lock the second finisher checks and installs here
        return real_install(directory, name, data)

    monkeypatch.setattr(mod.agy_home, "_install_state", install_while_the_second_finishes)
    results["first"] = mod.agy_home.finish_agy_home(plan, *reversed(finishers[0]))
    second.join()
    assert results["first"] == []
    kept = "jetski_state.pbtxt: changed by another session since staging; kept it"
    assert results["second"] == ["agy state not copied back: " + kept]
    assert (root / "jetski_state.pbtxt").read_text() == "first\n"

    # A lane holding the lock delays copy-back only for a bounded time.
    import fcntl
    staged, tmp_fd = stage_agy(mod, plan, tmp_path / "first")
    _rename_into(staged.path / ".gemini/antigravity-cli", "jetski_state.pbtxt", "held\n")
    monkeypatch.setattr(mod.agy_home, "AGY_COPY_BACK_WAIT", 0.2)
    with open(root / mod.agy_home.AGY_COPY_BACK_LOCK, "rb") as held:
        fcntl.flock(held, fcntl.LOCK_EX)
        warnings = mod.agy_home.finish_agy_home(plan, tmp_fd, staged)
    assert len(warnings) == 1 and "stayed locked" in warnings[0], warnings
    assert (root / "jetski_state.pbtxt").read_text() == "first\n" and not staged.path.exists()


def test_agy_staging_creates_an_empty_database_so_sqlite_writes_through_the_link(monkeypatch, tmp_path):
    import sqlite3
    mod = supervisor()
    _, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    private_tmp = tmp_path / "tmp"
    private_tmp.mkdir()
    plan = agy_plan()
    staged, tmp_fd = stage_agy(mod, plan, private_tmp)
    database = staged.path / ".gemini/antigravity-cli" / mod.agy_home.AGY_DATABASE
    assert database.readlink() == root / mod.agy_home.AGY_DATABASE
    with sqlite3.connect(database) as connection:
        connection.execute("create table t (v)")
        connection.execute("insert into t values (1)")
    connection.close()
    assert mod.agy_home.finish_agy_home(plan, tmp_fd, staged) == []
    with sqlite3.connect(root / mod.agy_home.AGY_DATABASE) as connection:
        assert connection.execute("select v from t").fetchall() == [(1,)]
    connection.close()
    # An existing database is left as it is.
    before = (root / mod.agy_home.AGY_DATABASE).read_bytes()
    mod.agy_home.finish_agy_home(plan, tmp_fd, mod.agy_home.stage_agy_home(plan, tmp_fd, private_tmp))
    assert (root / mod.agy_home.AGY_DATABASE).read_bytes() == before


RETRY_LANE = """
import os, pathlib, sys
tmp = pathlib.Path(os.environ["TMPDIR"])
victim = pathlib.Path(sys.argv[1])
shim = tmp / "provenant-shim"
for name in ("bin/ps", "process_info.py"):
    (shim / name).unlink()
    (shim / name).symlink_to(victim / "agentapi")
(tmp / "cache").rename(tmp / "cache-moved")
(tmp / "cache").symlink_to(victim)
sys.stderr.write("--effort is not supported for model fixture\\n")
sys.exit(1)
"""

SWAP_LANE = """
import json, os, pathlib, sys
tmp = pathlib.Path(os.environ["TMPDIR"])
tmp.rename(tmp.with_name("tmp-moved"))
tmp.symlink_to(sys.argv[1])
print(json.dumps({"type": "item.completed", "item": {"type": "agent_message", "text": "done"}}), flush=True)
print(json.dumps({"type": "turn.completed"}), flush=True)
"""


def test_effort_retry_never_writes_through_links_the_first_lane_planted(monkeypatch, tmp_path):
    """A lane forges an unsupported-effort diagnostic, so the same attempt runs again in its tmp."""
    mod = supervisor()
    preflight = importlib.import_module("skills.orchestrate.scripts.dispatch_preflight")
    monkeypatch.setattr(mod.agy_home, "needs_ps_shim", lambda plan: True)
    victim = tmp_path / "live-bin"
    victim.mkdir(mode=0o755)
    (victim / "agentapi").write_text("live shim\n", encoding="utf-8")
    attempt = tmp_path / "attempt"
    plan = fixture_plan(tmp_path, RETRY_LANE, run_dir=attempt)
    plan["argv"].append(str(victim))
    plan["effort"] = plan["route"]["effort"] = "high"
    plan["route"]["supported_efforts"] = ["low", "high"]

    record = mod.execute(plan, attempt / "result.md")
    retry = preflight.effort_retry_plan(plan, record)
    assert retry is not None, record
    retry["argv"] = plan["argv"]
    mod.execute(retry, attempt / "result.md")

    assert (victim / "agentapi").read_text() == "live shim\n"
    assert sorted(path.name for path in victim.iterdir()) == ["agentapi"]
    assert stat.S_IMODE(victim.stat().st_mode) == 0o755  # the planted cache link was not chmodded


def test_output_is_staged_in_the_held_tmp_after_the_lane_swaps_it(tmp_path):
    mod = supervisor()
    victim = tmp_path / "victim"
    victim.mkdir()
    attempt = tmp_path / "attempt"
    victim.chmod(0o500)  # a write that followed the swapped tmp would fail here
    plan = fixture_plan(tmp_path, SWAP_LANE, run_dir=attempt)
    plan["argv"].append(str(victim))
    record = mod.execute(plan, attempt / "result.md")
    assert record["status"] == "ok", record
    assert "done" in (attempt / "result.md").read_text()
    assert (attempt / "tmp").is_symlink() and list(victim.iterdir()) == []
    assert [path.name for path in (attempt / "tmp-moved").iterdir() if path.name.startswith(".provenant")] == []


AGY_LANE = """
import json, os, pathlib
home = pathlib.Path(os.environ["HOME"])
lane = home / ".gemini/antigravity-cli"
(lane / "jetski_state.pbtxt.tmp").write_text("lane\\n")
os.replace(lane / "jetski_state.pbtxt.tmp", lane / "jetski_state.pbtxt")
print(json.dumps({"status": "SUCCESS", "response": json.dumps(
    [str(home), (lane / "brain").is_symlink(), (lane / "conversations").is_symlink()])}))
"""


def test_agy_attempt_stages_after_state_preparation_and_cleans_up(monkeypatch, tmp_path):
    """Runs the real confinement_command; a pass-through stand-in replaces sandbox-exec itself."""
    mod = supervisor()
    home, root = agy_home_fixture(mod, monkeypatch, tmp_path)
    shutil.rmtree(root / "conversations")
    passthrough = tmp_path / "sandbox-exec"
    passthrough.write_text('#!/bin/sh\nshift 2\nexec "$@"\n', encoding="utf-8")
    passthrough.chmod(0o755)
    force_sandbox(monkeypatch, str(passthrough))
    workspace = tmp_path / "repo"
    workspace.mkdir()
    attempt = tmp_path / "attempt"
    attempt.mkdir()
    plan = fixture_plan(workspace, AGY_LANE, "agy", run_dir=attempt)
    plan["applied"]["confinement"] = "sandbox-exec"
    record = mod.execute(plan, attempt / "result.md")
    assert record["status"] == "ok", record
    lane_home, brain_linked, conversations_linked = json.loads((attempt / "result.md").read_text())
    assert Path(lane_home).parent == attempt / "tmp" and Path(lane_home).name.startswith("agy-home-")
    assert brain_linked and conversations_linked
    assert (root / "conversations").is_dir() and not (root / "conversations").is_symlink()
    assert (root / "jetski_state.pbtxt").read_text() == "lane\n"
    assert not Path(lane_home).exists()
    assert home.is_dir()


@pytest.mark.parametrize("adapter,project", [
    ("claude", "-srv-new-workspace"), ("cursor", "srv-new-workspace"),
])
def test_confined_launch_prepares_only_the_actual_project_root(monkeypatch, tmp_path, adapter, project):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    home.mkdir()
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = {"adapter": adapter, "mode": "read_only", "workspace_root": "/srv/new-workspace",
            "cwd": "/srv/new-workspace", "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    project_root = home / f".{adapter}/projects" / project
    assert not project_root.exists()
    command = mod.provider_confinement.confinement_command(plan, ["provider"])
    assert command[:2] == ["/usr/bin/sandbox-exec", "-p"]
    assert project_root.is_dir()
    assert list(project_root.iterdir()) == []
    assert f'(subpath "{project_root}")' not in command[2]
    assert [p.name for p in project_root.parent.iterdir()] == [project]


@pytest.mark.parametrize("adapter", ["claude", "cursor"])
def test_project_root_preparation_refuses_symlink_ancestors(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    provider = home / f".{adapter}"
    provider.mkdir(parents=True)
    target = tmp_path / "config"
    target.mkdir()
    (provider / "projects").symlink_to(target, target_is_directory=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = {"adapter": adapter, "cwd": "/srv/new-workspace", "applied": {"confinement": "sandbox-exec"}}
    with pytest.raises(PermissionError, match="provider state directory must be a real directory"):
        mod.provider_confinement.confinement_command(plan, ["provider"])
    assert list(target.iterdir()) == []


@pytest.mark.parametrize("mode", ["read_only", "worktree_write"])
@pytest.mark.parametrize("adapter", ["claude", "cursor", "kiro"])
def test_shared_npm_state_cannot_poison_reused_packages(monkeypatch, tmp_path, adapter, mode):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    workspace = home / "repo"
    workspace.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    profile = mod.provider_confinement.os_confinement_profile({
        "adapter": adapter, "mode": mode, "workspace_root": str(workspace), "cwd": str(workspace),
        "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    })
    writes = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-write* "))
    assert f'(subpath "{home / ".npm"}")' not in writes
    for name in ("_cacache", "_npx", "_prebuilds", "_tuf"):
        assert f'(subpath "{home / ".npm" / name}")' not in writes
    assert f'(subpath "{home / ".npm/_logs"}")' in writes
    assert f'(literal "{home / ".npm/_update-notifier-last-checked"}")' in writes
    assert any(line.startswith(f'(deny file-write-create (require-all (subpath "{home / ".npm"}")')
               and "(vnode-type SYMLINK)" in line for line in profile.splitlines())


KIRO_SUPPORT = "Library/Application Support/kiro-cli"
# Files in Kiro's support directory that the user's shell sources or that Kiro executes.
KIRO_EXECUTABLES = ("shell/zshrc.pre.zsh", "shell/bashrc.post.bash", "node", "bun", "tui.js",
                    "node.sha256", "kas/2.23.0-abc/node_modules/index.js", "run/chat-cli-2.23.0")


@pytest.mark.parametrize("mode", ["read_only", "worktree_write"])
def test_kiro_profile_writes_only_its_sign_in_state_not_shell_hooks_or_executables(monkeypatch, tmp_path, mode):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    workspace = home / "repo"
    workspace.mkdir(parents=True)
    support = home / KIRO_SUPPORT
    for relative in (*KIRO_EXECUTABLES, "data.sqlite3", ".refresh.lock", "history"):
        (support / relative).parent.mkdir(parents=True, exist_ok=True)
        (support / relative).write_text("original\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    plan = {"adapter": "kiro", "mode": mode, "workspace_root": str(workspace), "cwd": str(workspace),
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    profile = mod.provider_confinement.os_confinement_profile(plan)
    writes = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-write* "))
    assert f'(subpath "{support}")' not in writes
    assert "data\\.sqlite3[^/]*$" not in writes
    # SQLite's sidecars are files at fixed names, never directories a lane could fill.
    for name in ("data.sqlite3-wal", "data.sqlite3-shm", "data.sqlite3-journal"):
        assert f'(subpath "{support / name}")' not in writes, name
        assert f'(literal "{support / name}")' in writes, name
    if mode == "read_only":
        reads = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-read-data "))
        assert f'(subpath "{support}")' in reads
        # The engine resolves its app bundle through these launcher links, and needs nothing else there.
        assert f'(subpath "{home / ".local/bin"}")' not in reads
        for name in ("", "/kiro-cli", "/kiro-cli-chat", "/kiro-cli-term"):
            assert f'(literal "{home}/.local/bin{name}")' in reads
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if sys.platform != "darwin" or not sandbox_exec:
        return

    def run(script):
        return subprocess.run([sandbox_exec, "-p", profile, "/bin/sh", "-c", script],
                              capture_output=True, text=True)

    probe = run(f"printf x > '{support}/data.sqlite3-wal'")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert probe.returncode == 0, probe.stderr
    # SQLite creates and removes its own sidecars; kiro-cli rewrites its lock and database in place.
    assert run(f"rm '{support}/data.sqlite3-wal'").returncode == 0
    for name in ("data.sqlite3-shm", "data.sqlite3-journal"):
        assert run(f"printf x > '{support}/{name}' && rm '{support}/{name}'").returncode == 0, name
    # kiro-cli opens this lock for writing before it reads its token, or reports the token expired.
    assert run(f"printf x > '{support}/.refresh.lock'").returncode == 0
    assert run(f"printf x >> '{support}/data.sqlite3'").returncode == 0
    for relative in KIRO_EXECUTABLES:
        assert run(f"printf hijack > '{support}/{relative}'").returncode != 0, relative
        assert (support / relative).read_text(encoding="utf-8") == "original\n", relative
    assert run(f"printf x > '{support}/shell/new.zsh'").returncode != 0
    assert run(f"printf x > '{support}/kas/2.24.0-new'").returncode != 0
    assert run(f"printf x > '{support}/data.sqlite3-evil'").returncode != 0
    assert run(f"mkdir '{support}/data.sqlite3-wal'").returncode != 0
    assert run(f"mkfifo '{support}/data.sqlite3-shm'").returncode != 0
    assert not (support / "data.sqlite3-wal").exists() and not (support / "data.sqlite3-shm").exists()
    # No link that the user's unconfined kiro-cli would later follow into a shell hook.
    hook = support / "shell/zshrc.pre.zsh"
    for name in ("data.sqlite3-wal", "data.sqlite3-journal", ".refresh.lock", "data.sqlite3"):
        target = support / name
        assert run(f"ln -s '{hook}' '{target}'").returncode != 0, name
        assert run(f"ln '{hook}' '{target}'").returncode != 0, name
        assert not target.is_symlink(), name
    for name in (".refresh.lock", "data.sqlite3"):
        assert run(f"rm -f '{support}/{name}'").returncode != 0, name
        assert run(f"printf x > '{support}/data.sqlite3-wal' && mv '{support}/data.sqlite3-wal' '{support}/{name}'"
                   ).returncode != 0, name
        assert (support / name).is_file() and not (support / name).is_symlink(), name
    assert hook.read_text(encoding="utf-8") == "original\n"
    history = run(f"cat '{support}/history'")
    if mode == "read_only":
        # kiro-cli's prompt history spans every project; a read-only lane has no need of it.
        assert history.returncode != 0
        assert run(f"cat '{support}/data.sqlite3'").returncode == 0


STATE_WRITE_ENTRIES = [
    (adapter, entry)
    for adapter, state in importlib.import_module(
        "skills.orchestrate.scripts.provider_exec"
    ).provider_confinement.CONFINED_STATE.items()
    for entry in (*state.get("read_write", ()), *state.get("write_literal", ()), *state.get("write_in_place", ()))
    if not entry.endswith("*")
]


@pytest.mark.parametrize("mode", ["read_only", "worktree_write"])
@pytest.mark.parametrize("adapter,entry", STATE_WRITE_ENTRIES)
def test_a_link_planted_at_writable_provider_state_never_moves_the_grant(monkeypatch, tmp_path, adapter,
                                                                         entry, mode):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    workspace = home / "repo"
    workspace.mkdir(parents=True)
    subprocess.run(["git", "init", "-q", str(workspace)], check=True)
    target = home / "Library/Application Support/kiro-cli/node"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text("binary\n", encoding="utf-8")
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    plan = {"adapter": adapter, "mode": mode, "route": {}, "workspace_root": str(workspace),
            "cwd": str(workspace), "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    # Without a link, each grant names the state path itself.
    assert f'"{home / entry}"' in mod.provider_confinement.os_confinement_profile(plan)
    planted = home / entry
    planted.parent.mkdir(parents=True, exist_ok=True)
    if planted.exists():
        planted.unlink()
    planted.symlink_to(target)
    # An earlier lane planted a link there: the next profile refuses rather than granting its target.
    with pytest.raises(PermissionError, match="provider state must not be a symlink"):
        mod.provider_confinement.os_confinement_profile(plan)


def test_confined_kiro_refuses_a_missing_engine_with_a_fix(monkeypatch, tmp_path):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    workspace = tmp_path / "repo"
    workspace.mkdir()
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    launcher = bin_dir / "kiro-cli"
    launcher.write_text("#!/bin/sh\necho 'kiro-cli 9.9.9'\n", encoding="utf-8")
    launcher.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bin_dir}{os.pathsep}{os.environ['PATH']}")
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    route = {"resolved_model": "auto"}
    with pytest.raises(ValueError, match="kiro-cli 9.9.9 has not installed its engine.*fix: run kiro-cli once outside"):
        mod.build_plan("kiro", route, "hello", cwd=workspace, workspace_root=workspace)
    support = home / KIRO_SUPPORT
    (support / "kas/9.9.9-abc/node_modules").mkdir(parents=True)
    (support / "node").write_text("", encoding="utf-8")
    plan = mod.build_plan("kiro", route, "hello", cwd=workspace, workspace_root=workspace)
    assert plan["applied"]["confinement"] == "sandbox-exec"
    force_sandbox(monkeypatch, None)
    (support / "node").unlink()
    assert mod.build_plan("kiro", route, "hello", cwd=workspace, workspace_root=workspace)["applied"][
        "confinement"] == "none"


@pytest.mark.parametrize("adapter", ["claude", "codex", "agy", "opencode", "cursor", "kiro"])
def test_read_only_transcript_denies_follow_every_state_allow(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    workspace = tmp_path / "repo"
    workspace.mkdir()
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    plan = {"adapter": adapter, "mode": "read_only", "route": {}, "workspace_root": str(workspace),
            "cwd": str(workspace), "session_id": "11111111-2222-3333-4444-555555555555",
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    lines = mod.provider_confinement.os_confinement_profile(plan).splitlines()
    denied = [f'(subpath "{home / path}")' for path in mod.provider_confinement.EXTRA_DENIED_READS]
    last_deny = max(index for index, line in enumerate(lines)
                    if line.startswith("(deny file-read-data ") and all(rule in line for rule in denied))
    later = [line for line in lines[last_deny + 1:]
             if line.startswith("(allow file-read") and not line.startswith("(allow file-read-data file-read-metadata ")]
    # Only a Claude lane's own session transcript reopens, so that it can resume.
    own = home / ".claude/projects" / re.sub(r"[^A-Za-z0-9]", "-", str(workspace.resolve()))
    session = own / plan["session_id"]
    expected = [f'(allow file-read-data (literal "{session}.jsonl") (subpath "{session}"))']
    assert later == (expected if adapter == "claude" else [])
    # Only a UUID names a session, so no id can reopen the project's memory or another directory.
    for session_id in ("memory", "..", "s-1"):
        plan["session_id"] = session_id
        assert not any(str(own) in line for line in mod.provider_confinement.os_confinement_profile(plan).splitlines())
    del plan["session_id"]
    lines = mod.provider_confinement.os_confinement_profile(plan).splitlines()
    assert not any(str(own) in line for line in lines)


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_claude_read_only_lane_cannot_read_other_projects_transcripts(monkeypatch, tmp_path):
    mod = supervisor()
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    home = (tmp_path / "home").resolve()
    (home / ".claude/projects/other").mkdir(parents=True)
    (home / ".claude/projects/other/session.jsonl").write_text("transcript\n", encoding="utf-8")
    (home / ".claude/settings.json").write_text("{}\n", encoding="utf-8")
    workspace = tmp_path / "repo"
    workspace.mkdir()
    own = home / ".claude/projects" / re.sub(r"[^A-Za-z0-9]", "-", str(workspace.resolve()))
    mine = "11111111-2222-3333-4444-555555555555"
    (own / "memory").mkdir(parents=True)
    (own / mine).mkdir()
    (own / f"{mine}.jsonl").write_text("mine\n", encoding="utf-8")
    (own / f"{mine}/subagent.jsonl").write_text("mine\n", encoding="utf-8")
    (own / "chair.jsonl").write_text("chair\n", encoding="utf-8")
    (own / "memory/MEMORY.md").write_text("memory\n", encoding="utf-8")
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    plan = {"adapter": "claude", "mode": "read_only", "route": {}, "workspace_root": str(workspace),
            "cwd": str(workspace), "session_id": "66666666-7777-8888-9999-000000000000", "resume_session": mine,
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    profile = mod.provider_confinement.os_confinement_profile(plan)

    def run(script):
        return subprocess.run([sandbox_exec, "-p", profile, "/bin/sh", "-c", script],
                              capture_output=True, text=True)

    probe = run(f"cat '{home}/.claude/settings.json'")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert probe.stdout == "{}\n"
    assert run(f"cat '{home}/.claude/projects/other/session.jsonl'").returncode != 0
    # Its own session stays readable, so the lane can resume; the project's other sessions do not.
    assert run(f"cat '{own}/{mine}.jsonl'").stdout == "mine\n"
    assert run(f"cat '{own}/{mine}/subagent.jsonl'").stdout == "mine\n"
    assert run(f"cat '{own}/chair.jsonl'").returncode != 0
    assert run(f"cat '{own}/memory/MEMORY.md'").returncode != 0


def test_claude_project_dir_matches_claude_naming_including_long_paths():
    mod = supervisor()
    home = Path("/srv/someone")
    projects = home / ".claude/projects"
    assert mod.provider_confinement._claude_project_dir(home, "/srv/someone/Repos/my_app.v2") == (
        projects / "-srv-someone-Repos-my-app-v2")
    # Claude replaces each UTF-16 code unit, so a character outside the BMP becomes two dashes.
    assert mod.provider_confinement._claude_project_dir(home, "/srv/someone/a\U0001F600b").name == "-srv-someone-a--b"
    # A long name keeps 200 characters plus Claude's 32-bit string hash in base 36; checked against
    # the directory Claude Code 2.1.285 created for this path.
    scratch = ("/private/tmp/claude-501/-Users-user-Repos-provenant/"
               "76b874fc-49eb-4d7e-9fbb-8ff3e570ed9f/scratchpad/" + "x" * 120 + "/" + "y" * 60)
    assert mod.provider_confinement._claude_project_dir(home, scratch).name == (
        re.sub(r"[^A-Za-z0-9]", "-", scratch)[:200] + "-qpcnov")
    # Two long paths that share their first 200 characters get different, exact directories.
    first = mod.provider_confinement._claude_project_dir(home, "/srv/someone/" + "a" * 220 + "/one")
    second = mod.provider_confinement._claude_project_dir(home, "/srv/someone/" + "a" * 220 + "/two")
    assert first != second and first.name[:200] == second.name[:200]


def test_sbpl_filter_star_escapes_regex_without_resolving_symlink_target(tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    target = tmp_path / "target with [regex].json"
    target.write_text("secret", encoding="utf-8")
    link = tmp_path / "link with [regex].json"
    link.symlink_to(target)

    rule = supervisor.provider_confinement._sbpl_filter(str(link) + "*")

    assert rule == f'(regex #"^{link.parent}/link with \\[regex\\]\\.json[^/]*$")'


def test_read_only_confinement_paths_escape_sbpl_literals(tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / 'space"and\\slash'
    cwd = root / "sub"
    cwd.mkdir(parents=True)
    plan = {
        "adapter": "opencode", "mode": "read_only", "workspace_root": str(root),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }
    profile = supervisor.provider_confinement.os_confinement_profile(plan)
    assert 'space\\"and\\\\slash' in profile


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_sandbox_exec_profile_denies_home_reads_and_writes_but_keeps_provider_state(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    sandbox_exec = supervisor.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    home = (tmp_path / "home").resolve()
    (home / ".ssh").mkdir(parents=True)
    (home / ".ssh/id").write_text("key\n", encoding="utf-8")
    (home / "state").mkdir()
    (home / "creds.json").write_text("token\n", encoding="utf-8")
    cwd = home / "repo/sub"
    cwd.mkdir(parents=True)
    (cwd / "note.txt").write_text("note\n", encoding="utf-8")
    (home / ".gitconfig").write_text("[user]\n\tname = home-config\n", encoding="utf-8")
    xdg_git = home / ".config/git"
    xdg_git.mkdir(parents=True)
    (xdg_git / "config").write_text("[user]\n\temail = xdg-config@example.invalid\n", encoding="utf-8")
    subprocess.run(["git", "init", "-q", str(cwd)], check=True)
    monkeypatch.setattr(supervisor.Path, "home", lambda: home)
    monkeypatch.setattr(
        supervisor.provider_confinement, "CONFINED_STATE",
        {"agy": {"read_write": ("state", "creds.json*"), "read": ()}},
    )
    plan = {
        "adapter": "agy", "mode": "read_only", "workspace_root": str(home / "repo"),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }
    profile = supervisor.provider_confinement.os_confinement_profile(plan)

    def run(script):
        return subprocess.run(
            [sandbox_exec, "-p", profile, "/bin/sh", "-c", script], capture_output=True, text=True,
            env={**os.environ, "HOME": str(home), "XDG_CONFIG_HOME": str(home / ".config")},
        )

    probe = run(f"cat {cwd}/note.txt")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert probe.stdout == "note\n"
    config = run(
        f"git -C '{cwd}' config user.name && git -C '{cwd}' config user.email"
    )
    assert config.returncode == 0, config.stderr
    assert config.stdout.splitlines() == ["home-config", "xdg-config@example.invalid"]
    status = run(f"git -C '{cwd}' status --short")
    assert status.returncode == 0, status.stderr
    assert run(f"cat {home}/.ssh/id").returncode != 0
    assert run(f"echo x > {home}/written").returncode != 0
    assert not (home / "written").exists()
    assert run(f"echo x > {cwd}/written").returncode != 0
    assert run(f"echo x > {home}/state/log && cat {home}/creds.json").stdout == "token\n"
    assert run(f"echo y > {home}/creds.json.tmp").returncode == 0


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_sandbox_exec_keeps_add_dir_under_user_temp_read_only(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    sandbox_exec = supervisor.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    user_temp = (tmp_path / "T").resolve()
    add_dir = user_temp / "extra"
    cwd = (tmp_path / "workspace").resolve()
    add_dir.mkdir(parents=True)
    cwd.mkdir()
    (add_dir / "note.txt").write_text("note\n", encoding="utf-8")
    plan = {
        "adapter": "agy", "mode": "read_only", "workspace_root": str(cwd),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]},
    }
    profile = supervisor.provider_confinement.os_confinement_profile(plan)

    def run(script):
        return subprocess.run([sandbox_exec, "-p", profile, "/bin/sh", "-c", script], capture_output=True, text=True)

    probe = run(f"cat {add_dir}/note.txt")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert probe.stdout == "note\n"
    assert run(f"echo blocked > {add_dir}/written.txt").returncode != 0
    assert not (add_dir / "written.txt").exists()


def test_agy_read_only_guarantee_tracks_os_confinement(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    confined = supervisor.build_plan("agy", {"resolved_model": "gemini-test"}, "prompt", cwd=tmp_path, workspace_root=tmp_path)
    assert confined["applied"]["confinement"] == "sandbox-exec"
    assert confined["applied"]["guarantee"] == "best_effort"
    force_sandbox(monkeypatch, None)
    unconfined = supervisor.build_plan("agy", {"resolved_model": "gemini-test"}, "prompt", cwd=tmp_path, workspace_root=tmp_path)
    assert unconfined["applied"]["confinement"] == "none"
    assert unconfined["applied"]["guarantee"] == "prompt_only"
    assert any("writes are unconfined" in warning for warning in unconfined["warnings"])


@pytest.mark.parametrize(
    "adapter,sandbox,expected",
    [
        pytest.param(adapter, None, "warn", id=f"{adapter}-unconfined")
        for adapter in ("claude", "cursor", "kiro", "agy", "opencode")
    ]
    + [pytest.param("codex", None, "no-warning", id="codex-native")]
    + [pytest.param(adapter, "/usr/bin/sandbox-exec", "no-warning", id=f"{adapter}-sandbox")
       for adapter in ("agy", "opencode")],
)
def test_read_only_cwd_warning_tracks_adapter_and_confinement(monkeypatch, tmp_path, adapter, sandbox, expected):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / "workspace"
    cwd = root / "sub"
    cwd.mkdir(parents=True)
    force_sandbox(monkeypatch, sandbox)

    plan = supervisor.build_plan(
        adapter, {"resolved_model": "fixture"}, "prompt", cwd=cwd, workspace_root=root,
    )

    warning = f"{adapter} read_only: cwd is not a read boundary"
    if expected == "warn":
        assert plan["warnings"].count(warning) == 1
    elif adapter == "codex":
        assert not any("read boundary" in item for item in plan["warnings"])
    else:
        assert warning not in plan["warnings"]
def test_codex_read_only_cwd_below_workspace_does_not_warn(monkeypatch, tmp_path):
    """Codex's native read-only sandbox reads everywhere, so a cwd bounds nothing whatever it is."""
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / "workspace"
    cwd = root / "sub"
    cwd.mkdir(parents=True)
    force_sandbox(monkeypatch, None)

    plan = supervisor.build_plan(
        "codex", {"resolved_model": "fixture"}, "prompt", cwd=cwd, workspace_root=root,
    )

    assert not any("read boundary" in warning for warning in plan["warnings"])


@pytest.mark.parametrize("adapter", ["agy", "opencode"])
def test_read_only_cwd_warning_is_omitted_with_os_read_confinement(
    monkeypatch, tmp_path, adapter,
):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / "workspace"
    cwd = root / "sub"
    cwd.mkdir(parents=True)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")

    plan = supervisor.build_plan(
        adapter, {"resolved_model": "fixture"}, "prompt", cwd=cwd, workspace_root=root,
    )

    assert f"{adapter} read_only: cwd is not a read boundary" not in plan["warnings"]


def test_os_confinement_opt_out_disables_sandbox_exec(monkeypatch):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    monkeypatch.setattr(supervisor.sys, "platform", "darwin")
    monkeypatch.setattr(supervisor.shutil, "which", lambda _name: "/usr/bin/sandbox-exec")
    monkeypatch.setenv("PROVENANT_NO_OS_CONFINEMENT", "1")
    assert supervisor.provider_confinement._sandbox_exec_path() is None


def test_refused_sandbox_exec_falls_back_to_unconfined_with_warning(monkeypatch):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    monkeypatch.setattr(supervisor.sys, "platform", "darwin")
    monkeypatch.delenv("PROVENANT_NO_OS_CONFINEMENT", raising=False)
    monkeypatch.setattr(supervisor.shutil, "which", lambda _name: "/usr/bin/sandbox-exec")
    refused = subprocess.CompletedProcess([], 71, "", "sandbox-exec: sandbox_apply: Operation not permitted\n")
    monkeypatch.setattr(supervisor.subprocess, "run", lambda *_a, **_k: refused)
    supervisor.provider_confinement._sandbox_exec_usable.cache_clear()
    try:
        assert supervisor.provider_confinement._sandbox_exec_path() is None
    finally:
        supervisor.provider_confinement._sandbox_exec_usable.cache_clear()


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_sandbox_exec_profile_confines_reads_to_temp_subdirectory(tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    sandbox_exec = supervisor.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    root = tmp_path / "workspace"
    cwd = root / "sub"
    cwd.mkdir(parents=True)
    allowed = cwd / "allowed.txt"
    denied = root / "denied.txt"
    allowed.write_text("allowed\n", encoding="utf-8")
    denied.write_text("denied\n", encoding="utf-8")
    plan = {
        "adapter": "agy", "mode": "read_only", "workspace_root": str(root),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }
    profile = supervisor.provider_confinement.os_confinement_profile(plan)
    permitted = subprocess.run([sandbox_exec, "-p", profile, "/bin/cat", str(allowed)], capture_output=True, text=True)
    if permitted.returncode and any(word in permitted.stderr.lower() for word in ("sandbox_apply", "operation not permitted")):
        pytest.skip("sandbox_apply is refused in this test environment")
    assert permitted.returncode == 0, permitted.stderr
    assert permitted.stdout == "allowed\n"
    blocked = subprocess.run([sandbox_exec, "-p", profile, "/bin/cat", str(denied)], capture_output=True, text=True)
    assert blocked.returncode != 0


def supervisor():
    return importlib.import_module("skills.orchestrate.scripts.provider_exec")


def provider_output():
    return importlib.import_module("skills.orchestrate.scripts.provider_output")


def test_daily_catalogue_notes_are_not_promoted_back_to_dispatch_warnings(tmp_path, monkeypatch, capsys):
    router = importlib.import_module('scripts.model_route')
    monkeypatch.setenv('AGENT_FABRIC_STATE_ROOT', str(tmp_path / 'state'))
    monkeypatch.setenv('PROVENANT_NO_OS_CONFINEMENT', '1')
    note = router.model_route_live._NEWER_NOTE.format('fixture-new-model')
    for first in (True, False):
        router.emit({'status': 'ok', 'resolved_model': 'fixture', 'notes': ['ordinary route note'],
                     'warnings': [note]}, 0)
        route = json.loads(capsys.readouterr().out)
        plan = supervisor().build_plan('codex', route, 'hello', cwd=tmp_path, workspace_root=tmp_path)
        assert (note in plan['warnings']) is first
        assert 'ordinary route note' in plan['warnings']
        assert note in plan['route']['notes']


# Expected outcomes are independent of the production rule registry.
_CLASSIFIER_DIAGNOSTICS = [
    ("You've hit your usage limit", "usage_limited", "usage_limited", "usage_limited"),
    ("Individual quota reached", "usage_limited", "usage_limited", "usage_limited"),
    ("rate limit exceeded; HTTP 429", "rate_limited", "rate_limited", "rate_limited"),
    ("Login expired; not logged in", "auth_required", "auth_required", "auth"),
    ("model not found", "model_unavailable", "model_unavailable", "model_unavailable"),
    ('error: invalid model selection (--model "gemini-3.8-pro" --effort "high"): --effort is not supported for model "gemini-3.8-pro"', "rejected", "invalid_input", "provider_error"),
    ("invalid model selection; quota exceeded", "rejected", "invalid_input", "provider_error"),
    ("permission denied", "permission_blocked", "permission_blocked", "sandbox_denied"),
    ("permission denied; HTTP 429", "permission_blocked", "permission_blocked", "sandbox_denied"),
    ("HTTP 403 forbidden; HTTP 429", "rate_limited", "rate_limited", "rate_limited"),
    ("HTTP 403 forbidden", "permission_blocked", "permission_blocked", "sandbox_denied"),
    ("You've hit your weekly limit", "usage_limited", "usage_limited", "usage_limited"),
    ("529 overloaded", "rate_limited", "rate_limited", "rate_limited"),
]


_CLASSIFIER_QUOTA_CASES = [
    *[(adapter, f"429 RESOURCE_EXHAUSTED: Quota exceeded for requests per {window}", status)
      for adapter in ("agy", "codex", "opencode", "claude")
      for window, status in [("day", "usage_limited"), ("minute", "rate_limited"), ("second", "rate_limited")]],
    *[("agy", message if "RESOURCE_EXHAUSTED" in message else "429 RESOURCE_EXHAUSTED: " + message, status)
      for message, status in [
          ("Quota exceeded: requests per day. retry in 2 per minute docs", "usage_limited"),
          ("Quota exceeded: requests per hour. See per minute docs", "usage_limited"),
          ("Quota exceeded for GenerateRequestsPerMinutePerProjectPerModel", "rate_limited"),
          ("Quota exceeded for GenerateRequestsPerDayPerProjectPerModel", "usage_limited"),
          ("Quota exceeded for RequestsPerHour and RequestsPerMinute", "usage_limited"),
          ('{"error":{"message":"Quota exceeded for requests per minute"},"docs":"limits per day"}', "rate_limited"),
          ("RESOURCE_EXHAUSTED: limit: 60 per minute", "rate_limited"),
          ('RESOURCE_EXHAUSTED: Quota metric "generativelanguage.googleapis.com/generate_requests" per minute', "rate_limited"),
          ("RESOURCE_EXHAUSTED: limit: 60 per day", "usage_limited"),
      ]],
]


@pytest.mark.parametrize("adapter,stdout,stderr,exit_code,status,signature,expected_class", [
    *[(adapter, "", text, 1, status, signature, expected_class)
      for adapter in ("claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot")
      for text, status, signature, expected_class in _CLASSIFIER_DIAGNOSTICS],
    *[(adapter, "", message, 1, status, status, status)
      for adapter, message, status in _CLASSIFIER_QUOTA_CASES],
    # Applicability and adapter-specific precedence are part of the contract.
    ("codex", "", "try again at 2 AM; permission denied", 1, "usage_limited", "usage_limited", "usage_limited"),
    ("agy", "", "Resets in 167h; permission denied", 1, "usage_limited", "usage_limited", "usage_limited"),
    ("agy", "", "Resets in 167h", 1, "usage_limited", "usage_limited", "usage_limited"),
    ("claude", "", "try again at 2 AM", 1, "failed", None, "provider_error"),
    ("cursor", "", "Login expired; permission denied", 1, "auth_required", "auth_required", "auth"),
    ("opencode", "", "EPERM; Variant unavailable for opencode/x: high", 1, "permission_blocked", "permission_blocked", "sandbox_denied"),
    ("opencode", "", "Variant unavailable for opencode/x: high; provider.no-route", 1, "rejected", "invalid_input", "provider_error"),
    *[("opencode", json.dumps({"type": "error", "sessionID": "ses_1", "error": {"type": "provider.no-route", "message": message}}), "", 1, status, signature, expected_class)
      for message, status, signature, expected_class in [
          ("Model unavailable: opencode/nope", "model_unavailable", "model_unavailable", "model_unavailable"),
          ("Invalid model reference: bogus", "model_unavailable", "model_unavailable", "model_unavailable"),
          ("Variant unavailable for opencode/x: bogusvar", "rejected", "invalid_input", "provider_error"),
      ]],
    ("opencode", json.dumps({"type": "error", "sessionID": "ses_1", "error": {
        "type": "provider.quota", "message": "Rate limit exceeded. Please try again later.", "status": 429,
        "response": {"body": json.dumps({"type": "error", "error": {"type": "FreeUsageLimitError"}})}}}),
        "", 1, "usage_limited", "usage_limited", "usage_limited"),
    ("opencode", "Explain the rate limit algorithm", "", 1, "failed", None, "provider_error"),
    *[(adapter, '{"type":"result","result":"DONE"}', "ls: /root: Permission denied", 0, "ok", None, "none")
      for adapter in ("claude", "codex", "opencode")],
    *[(adapter, json.dumps({"type": "text", "text": message}), "", 0, "ok", None, "none")
      for adapter, message in [("opencode", "Model at capacity; provider overloaded"),
                               ("claude", "Discuss the weekly usage limit and quota exceeded errors")]],
    ("opencode", json.dumps({"type": "text", "text": "DONE"}),
     "ERROR src/a.ts(3,1): error TS2769: No overload matches this call.", 1, "ok", "exit_after_output", "none"),
])
def test_classifier_contract(adapter, stdout, stderr, exit_code, status, signature, expected_class):
    parsed = provider_output().parse_output(adapter, stdout, stderr, exit_code)
    assert (parsed["status"], parsed["signature"]) == (status, signature)
    assert provider_output().error_class(parsed["status"], parsed["signature"]) == expected_class
    assert len(parsed["excerpt"]) <= 200
    if "RESOURCE_EXHAUSTED" in stderr:
        assert stderr in parsed["excerpt"]


@pytest.mark.parametrize("adapter,shape", [
    ("agy", "fixture"), ("claude", "fixture"), ("codex", "fixture"),
    ("opencode", "event"), ("opencode", "stderr"),
])
@pytest.mark.parametrize("message", ["Model at capacity", "overloaded_error: Overloaded"])
def test_capacity_and_overload_are_retryable_provider_failures(tmp_path, adapter, shape, message):
    if shape == "fixture":
        fixture = json.loads((ROOT / f"tests/fixtures/fabric-v1/provider-{adapter}.json").read_text())
        event = json.loads(fixture["stdout"])
        if adapter == "agy":
            event.update(status="ERROR", response="", error=message)
        elif adapter == "claude":
            event["result"] = message
        else:
            event["error"]["message"] = message
        stdout, stderr, exit_code = json.dumps(event), "", fixture["exit"]
    elif shape == "event":
        stdout = json.dumps({"type": "error", "error": {"type": "provider.error", "message": message}})
        stderr, exit_code = "", 0
    else:
        # OpenCode can exit 1 after emitting text. A capacity diagnostic must
        # defeat its otherwise usable-output exception.
        stdout = json.dumps({"type": "text", "part": {"text": "Working"}})
        stderr, exit_code = message, 1
    code = f"import sys; print({stdout!r}); print({stderr!r}, file=sys.stderr); sys.exit({exit_code})"
    record = supervisor().execute(fixture_plan(tmp_path, code, adapter), tmp_path / "result.md")
    assert record["status"] == "rate_limited"
    assert record["retryable"] is True
    assert record["error_class"] == "rate_limited"
    assert record["evidence"]["signature"] == "rate_limited"


def test_invalid_effort_is_rejected_with_cli_fix_and_no_cooldown(tmp_path):
    from skills.orchestrate.scripts.fabric_records import write_cooldown

    message = ('error: invalid model selection (--model "gemini-3.8-pro-high" '
               '--effort "high"): --effort is not supported for model "gemini-3.8-pro-high"')
    code = f"import sys; print({message!r}, file=sys.stderr); sys.exit(1)"
    row = supervisor().execute(fixture_plan(tmp_path, code, "agy"), tmp_path / "result.md")
    assert row["status"] == "rejected"
    assert row["error"] == "invalid_input"
    assert row["evidence"]["signature"] == "invalid_input"
    assert row["fix"] == message
    cooldowns = tmp_path / "cooldowns.json"
    write_cooldown(row, path=cooldowns)
    assert not cooldowns.exists()


@pytest.mark.parametrize(
    "fixture", sorted((ROOT / "tests/fixtures/fabric-v1").glob("provider-*.json"))
)
def test_provider_golden(fixture):
    data = json.loads(fixture.read_text())
    for case in [data, *data.get("cases", [])]:
        result = provider_output().parse_output(
            case["adapter"], case["stdout"], case.get("stderr", ""), case["exit"]
        )
        for key, value in case["expected"].items():
            assert result[key] == value


def test_unregistered_model_failure_fix_names_adapter_models(tmp_path, monkeypatch):
    exec_routing = importlib.import_module("skills.orchestrate.scripts.exec_routing")
    monkeypatch.setattr(
        exec_routing,
        "snapshot",
        lambda: (_ for _ in ()).throw(AssertionError("snapshot subprocess called")),
    )
    plan = fixture_plan(tmp_path, 'import sys; print("model not found", file=sys.stderr); sys.exit(1)', "agy")
    plan["requested_model"] = "gemini-3.8-pro"
    plan["route"]["identity_source"] = "passed-through"
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "model_unavailable"
    assert record["fix"] == (
        "choose a registered model: gemini-3.8-flash, "
        "claude-opus-4-6-thinking, claude-sonnet-4-6"
    )


def test_model_families_load_catalog_without_snapshot(monkeypatch):
    exec_routing = importlib.import_module("skills.orchestrate.scripts.exec_routing")
    monkeypatch.setattr(
        exec_routing,
        "snapshot",
        lambda: (_ for _ in ()).throw(AssertionError("snapshot subprocess called")),
    )
    assert exec_routing.model_families("claude-sonnet-5-5") == ("anthropic",)


def test_structured_result_and_question_take_precedence_over_prose():
    events = "\n".join(
        map(
            json.dumps,
            [
                {
                    "type": "system",
                    "subtype": "init",
                    "session_id": "s1",
                    "model": "opus",
                },
                {
                    "type": "result",
                    "is_error": False,
                    "result": "```\nQUESTION: Target main or release/3?\n```",
                },
            ],
        )
    )
    parsed = provider_output().parse_output("claude", events)
    assert parsed["status"] == "input_required"
    assert parsed["question"] == "Target main or release/3?"
    assert parsed["session_id"] == "s1"
    assert parsed["observed_model"] == "opus"


def test_claude_session_comes_only_from_its_own_events_not_nested_tool_output():
    events = [
        {"type": "system", "subtype": "init", "session_id": "s1", "model": "opus"},
        {"type": "result", "is_error": False, "result": "done", "session_id": "s1"},
        {"type": "user", "message": {"content": [{"type": "tool_result", "content": {"session_id": "other"}}]},
         "tool_use_result": {"session_id": "other"}, "session_id": "s1"},
    ]
    parsed = provider_output().parse_output("claude", "\n".join(map(json.dumps, events)))
    assert parsed["session_id"] == "s1"


def test_reconnecting_is_nonfatal_and_quota_discussion_is_not_an_error():
    events = "\n".join(
        map(
            json.dumps,
            [
                {"type": "error", "message": "Reconnecting… 1/5"},
                {
                    "type": "item.completed",
                    "item": {
                        "type": "agent_message",
                        "text": "Explain quota exceeded errors",
                    },
                },
                {"type": "turn.completed"},
            ],
        )
    )
    assert provider_output().parse_output("codex", events)["status"] == "ok"


def test_reset_evidence_uses_provider_time_units():
    from datetime import UTC, datetime

    at = datetime.now(UTC).replace(hour=1, minute=0, second=0, microsecond=0)
    claude_reset = at + timedelta(hours=9)
    assert (
        provider_output().parse_output(
            "claude",
            json.dumps(
                {
                    "type": "rate_limit_event",
                    "rate_limit_info": {"status": "rejected", "resetsAt": int(claude_reset.timestamp())},
                }
            ),
            "",
            1,
            at=at,
        )["reset_at"]
        == claude_reset.strftime("%Y-%m-%dT%H:%M:%SZ")
    )
    assert (
        provider_output().parse_output(
            "agy", "", "RESOURCE_EXHAUSTED Resets in 167h", 3, at=at
        )["reset_at"]
        == (at + timedelta(hours=167)).strftime("%Y-%m-%dT%H:%M:%SZ")
    )
    assert (
        provider_output().parse_output(
            "codex", "", "Usage limit; try again at 2:00 AM", 1, at=at
        )["reset_at"]
        == at.replace(hour=2).strftime("%Y-%m-%dT%H:%M:%SZ")
    )


def fixture_plan(tmp_path, code, adapter="codex", **controls):
    mod = supervisor()
    with pytest.MonkeyPatch.context() as monkeypatch:
        monkeypatch.setenv("PROVENANT_NO_OS_CONFINEMENT", "1")
        plan = mod.build_plan(
            adapter,
            {
                "resolved_model": "fixture",
                "model_family": "openai",
                "endpoint_provider": "openai",
                "effort": "high",
            },
            "hello",
            cwd=tmp_path,
            workspace_root=tmp_path,
            **controls,
        )
    plan["argv"] = [sys.executable, "-u", "-c", code]
    plan["grace_seconds"] = 0.1
    return plan


@pytest.mark.parametrize("rollover", [False, True])
@pytest.mark.parametrize("ending", ["reconnect", "transport", "exit", "signal", "recovered"])
def test_codex_progress_requires_a_successful_terminal_result(tmp_path, monkeypatch, rollover, ending):
    module = supervisor()
    monkeypatch.setattr(module.provider_observer, "MAX_EVENTS_BYTES", 2048 if rollover else 20 * 1024 * 1024)
    events = [{"type": "item.completed", "item": {"type": "agent_message", "text": "Review in progress"}}]
    if ending in {"reconnect", "recovered"}:
        events.extend({"type": "error", "message": f"Reconnecting... {i}/5 (failed to load workspace requirements)"}
                      for i in range(1, 6))
    if ending == "transport":
        events.append({"type": "turn.failed", "error": {"message": "stream disconnected before completion"}})
    # Put the progress and errors outside the retained head/tail when rolling over.
    events.extend({"type": "telemetry", "data": "x" * 100} for _ in range(1000))
    if ending == "recovered":
        events.extend([
            {"type": "item.completed", "item": {"type": "agent_message", "text": "Final review"}},
            {"type": "turn.completed"},
        ])
    # Keep the large event stream out of argv: Linux limits each exec argument.
    events_path = tmp_path / "provider-events.jsonl"
    events_path.write_text("\n".join(map(json.dumps, events)))
    code = ("import os, signal\nfrom pathlib import Path\n"
            f"print(Path({str(events_path)!r}).read_text(), flush=True)\n")
    if ending == "signal":
        code += "os.kill(os.getpid(), signal.SIGKILL)\n"
    output = tmp_path / "result.md"
    record = module.execute(fixture_plan(tmp_path, code), output)
    if ending == "recovered":
        assert record["status"] == "ok"
        assert record["error_class"] == "none"
        assert not record.get("result_partial", False)
        assert "Final review" in output.read_text()
    else:
        assert record["status"] == "failed", record
        assert record["error_class"] == ("transient" if ending in {"reconnect", "transport"} else "provider_error")
        assert record["result_partial"] is True
        assert "Review in progress" in output.read_text()
        assert not record["certification_eligible"]


def test_codex_terminal_result_in_rolled_output_survives_later_signal(tmp_path, monkeypatch):
    module = supervisor()
    monkeypatch.setattr(module.provider_observer, "MAX_EVENTS_BYTES", 2048)
    padding = [{"type": "telemetry", "data": "x" * 100}] * 1000
    events = padding + [
        {"type": "item.completed", "item": {"type": "agent_message", "text": "Final review"}},
        {"type": "turn.completed"},
    ] + padding
    # Preserve capture rollover without exceeding Linux's per-argument exec limit.
    events_path = tmp_path / "provider-events.jsonl"
    events_path.write_text("\n".join(map(json.dumps, events)))
    code = ("import os, signal\nfrom pathlib import Path\n"
            f"print(Path({str(events_path)!r}).read_text(), flush=True)\n"
            "os.kill(os.getpid(), signal.SIGKILL)\n")
    output = tmp_path / "result.md"
    plan = fixture_plan(tmp_path, code)
    plan["grace_seconds"] = 10
    record = module.execute(plan, output)
    assert record["status"] == "interrupted"
    assert not record.get("result_partial", False)
    assert "Final review" in output.read_text()


@pytest.mark.parametrize("exit_code", [0, 1])
def test_opencode_usage_survives_provider_exit(tmp_path, exit_code):
    events = [
        {"type": "text", "part": {"text": "Full answer"}},
        {"type": "step_finish", "part": {"tokens": {"input": 71074, "output": 5493,
            "reasoning": 7838, "cache": {"read": 1009536, "write": 0}}}},
    ]
    code = "import sys; print(" + repr("\n".join(json.dumps(event) for event in events)) + "); sys.exit(" + str(exit_code) + ")"
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["evidence"]["exit"] == exit_code
    assert record["error_class"] == "none"
    assert (tmp_path / "result.md").read_text() == "Full answer"
    assert record["usage_source"] == "observed"
    if exit_code:
        assert record["evidence"]["signature"] == "exit_after_output"
        assert record["reason"] == "provider exited 1 after final assistant output"
        assert record["reason"] in record["warnings"]
    assert {key: record["context"][key] for key in ("input_tokens", "output_tokens", "cached_input_tokens",
                                                  "cache_write_tokens", "reasoning_tokens")} == {
        "input_tokens": 1080610, "output_tokens": 5493, "cached_input_tokens": 1009536,
        "cache_write_tokens": 0, "reasoning_tokens": 7838}


@pytest.mark.parametrize("logs,reason", [
    (["INFO starting", "WARN fabric MCP unavailable", "ERROR shutting down", "ERROR later"], "ERROR shutting down"),
    (["WARN first", "WARN later"], "WARN first"),
    (["timestamp=x level=INFO starting", "timestamp=x level=ERROR message=shutdown"],
     "timestamp=x level=ERROR message=shutdown"),
    (["INFO rate limit configuration loaded"], "provider exited 1 after final assistant output"),
])
def test_opencode_exit_after_output_reason_skips_info(tmp_path, logs, reason):
    event = {"type": "text", "part": {"text": "Final answer"}}
    code = ("import json, sys\nprint(" + repr(json.dumps(event)) + ")\n"
            + "print(" + repr("\n".join(logs)) + ", file=sys.stderr)\nsys.exit(1)\n")
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["evidence"]["signature"] == "exit_after_output"
    assert record["reason"] == reason
    assert (tmp_path / "result.md").read_text() == "Final answer"


@pytest.mark.parametrize("rollover", [False, True])
def test_opencode_sandbox_denial_after_text_remains_a_failure(tmp_path, monkeypatch, rollover):
    monkeypatch.setattr(supervisor().provider_observer, "MAX_EVENTS_BYTES", 2048 if rollover else 20 * 1024 * 1024)
    events = [
        *[{"type": "telemetry", "data": "x" * 100} for _ in range(100)],
        {"type": "text", "part": {"text": "Working narration"}},
    ]
    denial = "ERROR ConfigDiscovery.discover: EPERM: operation not permitted"
    code = ("import sys; print(" + repr("\n".join(map(json.dumps, events)))
            + "); print(" + repr(denial) + ", file=sys.stderr); sys.exit(1)")
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == "permission_blocked"
    assert record["error_class"] == "sandbox_denied"
    assert record["reason"] == denial


@pytest.mark.parametrize("rollover", [False, True])
def test_opencode_exit_after_output_keeps_final_step_text(tmp_path, monkeypatch, rollover):
    mod = supervisor()
    monkeypatch.setattr(mod.provider_observer, "MAX_EVENTS_BYTES", 2048 if rollover else 20 * 1024 * 1024)
    events = [
        {"type": "step_start", "part": {}},
        {"type": "text", "part": {"text": "Working narration"}},
        {"type": "tool_use", "part": {"tool": "read"}},
        *[{"type": "telemetry", "data": "x" * 100} for _ in range(100)],
        {"type": "step_start", "part": {}},
        {"type": "text", "part": {"text": "Final answer"}},
        {"type": "step_finish", "part": {"reason": "stop"}},
    ]
    code = "import sys; print(" + repr("\n".join(map(json.dumps, events))) + "); sys.exit(1)"
    record = mod.execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["evidence"]["signature"] == "exit_after_output"
    assert record["evidence"]["exit"] == 1
    assert (tmp_path / "result.md").read_text() == "Final answer"


@pytest.mark.parametrize("later_event", [
    {"type": "tool_use", "part": {"tool": "read"}},
    {"type": "step_start", "part": {}},
    {"type": "error", "error": {"message": "backend failed"}},
    {"type": "step_finish", "part": {"reason": "tool-calls"}},
])
def test_opencode_intermediate_output_does_not_mask_a_failure(tmp_path, later_event):
    events = [{"type": "text", "part": {"text": "Working narration"}}, later_event]
    code = "import sys; print(" + repr("\n".join(map(json.dumps, events))) + "); sys.exit(1)"
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == "failed"
    assert record["evidence"]["signature"] != "exit_after_output"


@pytest.mark.parametrize("log", ["INFO cli starting", "[INFO] cli starting", "2026-10-21T14:00:00Z INFO cli starting"])
def test_opencode_failure_reason_skips_info_only_stderr(tmp_path, log):
    code = "import sys; print(" + repr(log) + ", file=sys.stderr); sys.exit(1)"
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == "failed"
    assert record["reason"] == "provider exited 1 without final assistant output"


@pytest.mark.parametrize("rollover", [False, True])
def test_opencode_final_text_is_preserved_beside_a_structured_failure(tmp_path, monkeypatch, rollover):
    monkeypatch.setattr(supervisor().provider_observer, "MAX_EVENTS_BYTES", 2048 if rollover else 20 * 1024 * 1024)
    events = [
        {"type": "error", "error": {"message": "shutdown failed"}},
        *[{"type": "telemetry", "data": "x" * 100} for _ in range(100)],
        {"type": "text", "part": {"text": "Final answer"}},
        {"type": "step_finish", "part": {"reason": "stop"}},
        *[{"type": "telemetry", "data": "x" * 100} for _ in range(100)],
    ]
    code = ("import sys; print(" + repr("\n".join(map(json.dumps, events)))
            + "); print('ERROR shutdown failed', file=sys.stderr); sys.exit(1)")
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == "failed"
    assert (tmp_path / "result.md").read_text() == "Final answer"


@pytest.mark.parametrize("structured_error", [False, True])
def test_opencode_discovery_eperm_is_sandbox_denied(tmp_path, structured_error):
    denial = "ERROR ConfigDiscovery.discover: EPERM: operation not permitted, lstat '/repo/.claude'"
    event = {"type": "error", "error": {"message": "provider failed"}}
    code = "import sys; print(" + repr(denial) + ", file=sys.stderr); "
    if structured_error:
        code += "print(" + repr(json.dumps(event)) + "); "
    code += "sys.exit(1)"
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == ("failed" if structured_error else "permission_blocked")
    assert record["error_class"] == ("provider_error" if structured_error else "sandbox_denied")
    assert record["reason"] == ("provider failed" if structured_error else denial)


@pytest.mark.parametrize("message,status", [
    ("429 too many requests", "rate_limited"),
    ("Individual quota reached", "usage_limited"),
    ("401 unauthorized", "auth_required"),
])
@pytest.mark.parametrize("rollover", [False, True])
def test_opencode_structured_failure_wins_over_incidental_eperm(tmp_path, monkeypatch, message, status, rollover):
    monkeypatch.setattr(supervisor().provider_observer, "MAX_EVENTS_BYTES", 2048 if rollover else 20 * 1024 * 1024)
    events = [
        {"type": "error", "error": {"message": message}},
        *[{"type": "telemetry", "data": "x" * 100} for _ in range(100)],
        {"type": "text", "part": {"text": "Conversation before shutdown"}},
    ]
    code = ("import sys; print(" + repr("\n".join(map(json.dumps, events)))
            + "); print(" + repr("WARN MCP offline\nERROR discovery EPERM") + ", file=sys.stderr); sys.exit(1)")
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == status
    assert record["error_class"] == ("auth" if status == "auth_required" else status)
    assert record["reason"] == message


@pytest.mark.parametrize("adapter,events,expected", [
    ("codex", [{"type": "item.completed", "item": {"type": "agent_message", "text": "OK"}},
               {"type": "turn.completed", "usage": {"input_tokens": 12, "output_tokens": 3}}], "observed"),
    ("codex", [{"type": "turn.completed", "usage": {"input_tokens": 0, "output_tokens": 0}}], "observed"),
    ("codex", [{"type": "turn.completed", "usage": {}}], "missing"),
    ("kiro", [{"type": "metadata", "data": {"contextUsagePercentage": 7.7}}], "missing"),
    ("copilot", [], "missing"),
])
def test_receipt_usage_source_tracks_token_evidence_not_context_estimates(tmp_path, adapter, events, expected):
    code = "print(" + repr("\n".join(json.dumps(event) for event in events)) + ")"
    record = supervisor().execute(fixture_plan(tmp_path, code, adapter), tmp_path / "result.md")
    assert record["usage_source"] == expected


@pytest.mark.parametrize("status,signature,expected", [
    ("ok", None, "none"), ("succeeded", None, "none"), ("input_required", None, "none"),
    ("ok", "rate_limited", "none"),
    ("permission_blocked", "rate_limited", "sandbox_denied"),
    ("rate_limited", "auth_expired", "rate_limited"),
    ("lost", "empty_output", "provider_error"),
    ("failed", "ok", "provider_error"),
    ("failed", "provider_transport", "transient"),
    ("failed", "fabric_mcp_unavailable", "fabric_mcp_unavailable"),
    ("failed", "pinned_model_substitution", "model_unavailable"),
    ("future_status", "ok", "unknown"),
    ("rate_limited", None, "rate_limited"), ("usage_limited", "quota_exhausted", "usage_limited"),
    ("auth_required", "auth_expired", "auth"), ("model_unavailable", None, "model_unavailable"),
    ("timed_out", None, "timeout"), ("startup_timeout", "startup_watchdog", "timeout"),
    ("stalled", "idle_watchdog", "timeout"), ("partial", "print_timeout", "timeout"),
    ("permission_blocked", None, "sandbox_denied"),
    ("failed", "empty_output", "empty_output"), ("failed", "rate_limited", "rate_limited"),
    ("failed", "quota_exhausted", "usage_limited"), ("failed", "auth_expired", "auth"),
    ("failed", "model_unavailable", "model_unavailable"), ("failed", "wall_clock", "timeout"),
    ("failed", "permission_blocked", "sandbox_denied"), ("failed", None, "provider_error"),
    ("rejected", "invalid_input", "provider_error"), ("tool_missing", None, "provider_error"),
    ("cancelled", "cancel_requested", "cancelled"), ("interrupted", "provider_signal", "unknown"),
    ("partial", "output_truncated", "unknown"), ("quarantined", None, "unknown"),
    ("future_status", "future_signature", "unknown"),
])
def test_terminal_error_class_is_derived_from_status_and_signature(status, signature, expected):
    assert provider_output().error_class(status, signature) == expected


def test_attempt_private_temp_and_cache_environment(tmp_path):
    code = """import json, os
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':json.dumps({key: os.environ.get(key) for key in ('TMPDIR','TMP','TEMP','TMPPREFIX','XDG_CACHE_HOME')})}}))
print(json.dumps({'type':'turn.completed'}))
"""
    plan = fixture_plan(tmp_path, code)
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    expected = {key: str(tmp_path / "tmp" / "cache" if key == "XDG_CACHE_HOME" else tmp_path / "tmp")
                for key in ("TMPDIR", "TMP", "TEMP", "XDG_CACHE_HOME")}
    expected["TMPPREFIX"] = str(tmp_path / "tmp" / "zsh")
    assert (tmp_path / "tmp" / "cache").is_dir()
    assert not (tmp_path / "cache").exists()
    assert json.loads((tmp_path / "result.md").read_text()) == expected


@pytest.mark.parametrize("mode", ["read_only", "worktree_write"])
def test_only_codex_writers_inherit_the_configured_fabric_store(monkeypatch, tmp_path, mode):
    state = str(tmp_path / "fabric-state")
    monkeypatch.setenv("AGENT_FABRIC_STATE_DIRECTORY", state)
    code = """import json, os
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':json.dumps(os.environ.get('AGENT_FABRIC_STATE_DIRECTORY'))}}))
print(json.dumps({'type':'turn.completed'}))
"""
    plan = fixture_plan(tmp_path, code, mode=mode)
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert json.loads((tmp_path / "result.md").read_text()) == (state if mode == "worktree_write" else None)


def test_codex_capabilities_validate_the_codex_writer_envelope(monkeypatch, tmp_path):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    attempt = tmp_path / "runs/task/attempt-001"
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")

    plan = mod.build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
        mode="worktree_write", worktree=lane, sandbox="workspace-write", network=True,
        capabilities=["postgres", "browser"], run_dir=attempt,
    )

    common = Path(git(lane, "rev-parse", "--path-format=absolute", "--git-common-dir").strip())
    assert plan["applied"]["capabilities"] == ["browser", "postgres"]
    assert plan["applied"]["sandbox"] == "workspace-write"
    assert plan["applied"]["network"] is True
    assert plan["applied"]["confinement"] == "sandbox-exec"
    assert plan["applied"]["guarantee"] == "enforced"
    assert str(lane / ".agents") in plan["applied"]["add_dirs"]
    assert str(common) not in plan["applied"]["add_dirs"]
    assert str(common) not in plan["applied"]["write_boundary"]["writable_paths"]
    assert str(common / "objects") in plan["applied"]["write_boundary"]["writable_paths"]
    assert str(attempt.parent / "codex-home") in plan["applied"]["write_boundary"]["writable_paths"]
    explicit_common = mod.build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
        mode="worktree_write", worktree=lane, sandbox="workspace-write", network=True,
        capabilities=["postgres"], run_dir=attempt, add_dirs=[str(common)],
    )
    assert str(common) not in explicit_common["applied"]["add_dirs"]
    assert any("drops Git common directory add-dir" in warning for warning in explicit_common["warnings"])


@pytest.mark.parametrize(("field", "value", "expected"), [
    ("mode", "read_only", "Pass capabilities only with mode=worktree_write."),
    ("platform", "linux", "Pass capabilities only on macOS."),
    ("sandbox_exec", None, "Use capabilities only with usable sandbox-exec outside another sandbox."),
    ("network", False, "Pass network=true for Codex capabilities."),
    ("sandbox", "read-only", "Use sandbox workspace-write or full with Codex capabilities."),
])
def test_codex_capabilities_fail_closed_on_each_precondition(monkeypatch, tmp_path, field, value, expected):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    options = {
        "adapter": "codex", "mode": "worktree_write", "sandbox": "workspace-write",
        "network": True,
    }
    if field == "adapter": options["adapter"] = value
    elif field == "mode":
        options["mode"] = value
        options["sandbox"] = "read-only"
    elif field == "sandbox": options["sandbox"] = value
    elif field == "platform": monkeypatch.setattr(mod.sys, "platform", value)
    elif field == "sandbox_exec": force_sandbox(monkeypatch, value)
    elif field == "network": options["network"] = value
    worktree = lane if options["mode"] == "worktree_write" else None

    with pytest.raises(ValueError, match=expected):
        mod.build_plan(
            options["adapter"], {"resolved_model": "fixture"}, "hello", cwd=lane,
            workspace_root=tmp_path, mode=options["mode"], worktree=worktree,
            sandbox=options["sandbox"], network=options["network"], capabilities=["postgres"],
        )


def test_codex_capabilities_with_full_sandbox_are_accepted_with_a_warning(monkeypatch, tmp_path):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    monkeypatch.setattr(mod.sys, "platform", "linux")  # A full sandbox needs no confinement to grant them.
    plan = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
                          mode="worktree_write", worktree=lane, sandbox="full", network=True,
                          capabilities=["postgres", "browser"], run_dir=tmp_path / "attempt")
    assert plan["applied"]["sandbox"] == "full"
    assert plan["applied"]["capabilities"] == []
    assert plan["applied"]["confinement"] == "none"
    assert "codex_home" not in plan
    assert any("sandbox full" in warning and "capabilities" in warning for warning in plan["warnings"])


@pytest.mark.parametrize("mode", ["worktree_write", "read_only"])
def test_other_adapters_take_capabilities_without_a_grant(monkeypatch, tmp_path, mode):
    mod = supervisor()
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    _, lane = instruction_lane(tmp_path)
    attempt = tmp_path / "attempt"
    attempt.mkdir()
    plan = mod.build_plan("claude", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
                          cwd=lane if mode == "read_only" else None, mode=mode,
                          worktree=lane if mode == "worktree_write" else None,
                          capabilities=["browser", "postgres"], run_dir=attempt)
    assert plan["applied"]["capabilities"] == ["browser", "postgres"]
    assert "codex_home" not in plan
    # Their sandbox-exec profile leaves SysV IPC and Mach services open; only Chrome's temp moves.
    profile = mod.provider_confinement.os_confinement_profile(plan)
    assert "(deny mach-lookup)" not in profile and "(deny ipc-sysv*)" not in profile
    temporary = plan["applied"]["browser_tmpdir"]
    assert f'(subpath "{temporary}")' in next(
        line for line in profile.splitlines() if line.startswith('(allow file-write* '))
    if mode == "read_only":
        assert profile.index(f'(allow file-read-data (subpath "{attempt}") (subpath "{temporary}"))') > profile.index(
            '(deny file-read-data (subpath ')
    assert temporary in plan["applied"]["write_boundary"]["writable_paths"]
    plan["applied"]["confinement"] = "none"
    plan["argv"] = [sys.executable, "-u", "-c", "import json, os; from pathlib import Path; "
                    "root=Path(os.environ['MAC_CHROMIUM_TMPDIR']); "
                    "assert root.stat().st_mode & 0o777 == 0o700; "
                    "(root / '.org.chromium.Chromium.123456').mkdir(); "
                    "(root / '.org.chromium.Chromium.123456/data').write_text('browser state'); "
                    "print(json.dumps({'type':'result','result':str(root)}))"]
    record = mod.execute(plan, attempt / "result.md", env={**os.environ, "MAC_CHROMIUM_TMPDIR": "ambient"})
    assert record["status"] == "ok", record
    temporary = Path(record["applied"]["browser_tmpdir"])
    assert (attempt / "result.md").read_text() == str(temporary)
    assert temporary.parent == Path("/tmp").resolve()
    assert not temporary.exists()


def test_browser_tmp_cleanup_trouble_warns_without_discarding_the_result(monkeypatch, tmp_path):
    mod = supervisor()
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    _, lane = instruction_lane(tmp_path)
    attempt = tmp_path / "attempt"
    attempt.mkdir()
    plan = mod.build_plan("claude", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
                          mode="worktree_write", worktree=lane, capabilities=["browser"], run_dir=attempt)
    plan["applied"]["confinement"] = "none"
    # The lane swaps its directory for another one; cleanup must leave it and only warn. The new
    # directory is made while the old one exists, as tmpfs may otherwise reuse its inode.
    plan["argv"] = [sys.executable, "-u", "-c", "import json, os; from pathlib import Path; "
                    "root=Path(os.environ['MAC_CHROMIUM_TMPDIR']); aside=root.rename(str(root) + '-old'); "
                    "root.mkdir(); aside.rmdir(); "
                    "print(json.dumps({'type':'result','result':'done'}))"]
    record = mod.execute(plan, attempt / "result.md", env=dict(os.environ))
    temporary = Path(record["applied"]["browser_tmpdir"])
    try:
        assert record["status"] == "ok", record
        assert (attempt / "result.md").read_text() == "done"
        assert temporary.exists()
        assert any("was replaced" in warning for warning in record["warnings"])
    finally:
        temporary.rmdir()


@pytest.mark.parametrize("capabilities", [["other"], ["browser", "browser"], "browser"])
def test_codex_capabilities_reject_unknown_duplicate_and_non_list_values(monkeypatch, tmp_path, capabilities):
    mod = supervisor()
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    with pytest.raises(ValueError, match="list of distinct postgres or browser values"):
        mod.build_plan("codex", {}, "hello", cwd=tmp_path, workspace_root=tmp_path,
                       capabilities=capabilities)


def test_codex_capability_profile_keeps_git_narrow_and_grants_only_selected_features(monkeypatch, tmp_path):
    mod = supervisor()
    repo, lane = instruction_lane(tmp_path)
    home = (tmp_path / "source-codex-home").resolve()
    home.mkdir()
    (home / "auth.json").write_text("token", encoding="utf-8")
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    monkeypatch.setenv("CODEX_HOME", str(home))
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    attempt = tmp_path / "runs/task/attempt-001"
    common = Path(git(lane, "rev-parse", "--path-format=absolute", "--git-common-dir").strip())
    plan = mod.build_plan("codex", {}, "hello", workspace_root=tmp_path, mode="worktree_write",
                          worktree=lane, sandbox="workspace-write", network=True,
                          capabilities=["postgres"], run_dir=attempt)
    profile = mod.provider_confinement.os_confinement_profile(plan)

    assert '(deny network-outbound (remote unix-socket))' in profile
    socket_roots = [lane, *plan["applied"]["add_dirs"], attempt, plan["codex_home"]]
    expected_socket_allow = "(allow network-outbound " + " ".join(
        ['(remote unix-socket (path-literal "/private/var/run/mDNSResponder"))']
        + [f'(remote unix-socket (subpath "{Path(root).resolve()}"))' for root in socket_roots]
    ) + ")"
    assert expected_socket_allow in profile

    assert f'(deny file-write* (subpath "{common}")' in profile
    assert f'(subpath "{common / "objects"}")' in profile
    for path in (common / "refs/heads/lane", common / "refs/heads/lane.lock", common / "logs/refs/heads/lane"):
        assert f'(literal "{path}")' in profile
    for path in (common / "objects", common / "refs/heads/lane", common / "logs/refs/heads/lane"):
        assert str(path) in plan["applied"]["write_boundary"]["writable_paths"]
    for path in (common / "refs", common / "logs", common / "packed-refs"):
        assert str(path) not in plan["applied"]["write_boundary"]["writable_paths"]
    assert f'(allow file-write* (subpath "{common}")' not in profile
    assert f'(subpath "{attempt.parent / "codex-home"}")' in profile
    assert f'(literal "{(home / "auth.json").resolve()}")' in profile
    assert f'(subpath "{home}")' not in profile
    assert '(deny mach-lookup)' in profile
    assert '(deny mach-register)' in profile
    assert '(deny signal)' in profile
    assert '(allow signal (target same-sandbox))' in profile
    assert '(deny user-preference-write)' in profile
    for service in mod.provider_confinement.CODEX_BASE_MACH_SERVICES:
        assert f'(global-name "{service}")' in profile
    assert '(allow ipc-sysv-shm ipc-sysv-sem)' in profile
    assert 'MachPortRendezvousServer.' not in profile
    private = Path(git(lane, "rev-parse", "--path-format=absolute", "--absolute-git-dir").strip())
    assert str(private) in plan["applied"]["write_boundary"]["writable_paths"]
    assert f'(allow file-write* (subpath "{private}")' in profile
    read_only = [lane / ".git", *(private / name for name in ("config.worktree", "commondir", "gitdir"))]

    def denied(text):
        return "\n".join(line for line in text.splitlines() if line.startswith("(deny file-write* "))

    for path in read_only:
        assert f'(subpath "{path}")' in denied(profile)

    for adapter in ("codex", "claude"):
        without_capabilities = mod.build_plan(
            adapter, {}, "hello", workspace_root=tmp_path, mode="worktree_write",
            worktree=lane, sandbox="workspace-write", network=True, run_dir=attempt,
        )
        other_profile = mod.provider_confinement.os_confinement_profile(without_capabilities)
        assert "(remote unix-socket" not in other_profile
        # Every writer keeps the Git discovery files read-only, not only capability lanes.
        for path in read_only:
            assert f'(subpath "{path}")' in denied(other_profile)
        for name in ("config.worktree", "commondir", "gitdir"):
            assert f'(deny file-write* (literal "{private / name}"))' not in other_profile

    protected = tmp_path / "protected"
    plan["protected_paths"] = [str(protected)]
    protected_profile = mod.provider_confinement.os_confinement_profile(plan)
    assert protected_profile.rstrip().endswith(f'(deny file-read* (subpath "{protected}"))')

    browser = mod.build_plan("codex", {}, "hello", workspace_root=tmp_path, mode="worktree_write",
                             worktree=lane, sandbox="workspace-write", network=True,
                             capabilities=["browser"], run_dir=attempt)
    browser_profile = mod.provider_confinement.os_confinement_profile(browser)
    browser_tmp = browser["applied"]["browser_tmpdir"]
    assert f'(subpath "{browser_tmp}")' in next(
        line for line in browser_profile.splitlines() if line.startswith('(allow file-write* '))
    assert f'(remote unix-socket (subpath "{browser_tmp}"))' in browser_profile
    assert browser_tmp in browser["applied"]["write_boundary"]["writable_paths"]
    assert "browser_tmpdir" not in plan["applied"]
    for service in (mod.provider_confinement.CODEX_BASE_MACH_SERVICES
                    + mod.provider_confinement.CODEX_BROWSER_MACH_SERVICES):
        assert f'(global-name "{service}")' in browser_profile
    for prefix in mod.provider_confinement.CODEX_BROWSER_MACH_PORT_PREFIXES:
        assert f'(global-name-prefix "{prefix}")' in browser_profile
    assert '(allow mach-register' in browser_profile
    assert 'ipc-sysv-shm' not in browser_profile


def codex_options(plan):
    argv = plan["argv"]
    return {argv[index + 1] for index, arg in enumerate(argv) if arg == "-c"}


def mcp_table(plan):
    """The single `mcp_servers={...}` override, parsed as TOML the way Codex reads it."""
    import tomllib
    tables = [option for option in codex_options(plan) if option.startswith("mcp_servers=")]
    assert len(tables) <= 1, "one parent-table override, so a later one cannot replace an earlier"
    return tomllib.loads(tables[0])["mcp_servers"] if tables else {}


def test_codex_read_only_lane_disables_inherited_mcp_servers_and_writer_keeps_them(tmp_path):
    mod = supervisor()
    (Path(os.environ["CODEX_HOME"]) / "config.toml").write_text(
        '[mcp_servers.fabric]\ncommand = "x"\n[mcp_servers.docs]\nurl = "https://example.invalid/mcp"\n',
        encoding="utf-8")
    route = {"resolved_model": "fixture"}
    reader = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)
    options = codex_options(reader)
    assert mcp_table(reader) == {"fabric": {"enabled": False},
                                 "docs": {"default_tools_approval_mode": "approve"}}
    assert reader["applied"]["mcp_disabled"] == ["fabric"]
    _, worktree = sandbox_profile_worktree(tmp_path)
    writer = mod.build_plan("codex", route, "hello", mode="worktree_write", worktree=worktree,
                            workspace_root=worktree, network=True)
    assert mcp_table(writer) == {"fabric": {"default_tools_approval_mode": "approve"},
                                 "docs": {"default_tools_approval_mode": "approve"}}


def test_codex_read_only_lane_disables_hooks_and_every_discovered_stdio_server(monkeypatch, tmp_path):
    """Project-layer and plugin stdio servers appear only in Codex's own inventory, not the user file."""
    mod = supervisor()
    codex = mod.profile("codex")
    seen = []

    def discover(cwd):
        seen.append(cwd)
        return {"projsrv": {"exec": True, "plugin": False}, "weird.name": {"exec": True, "plugin": False},
                "projurl": {"exec": False, "plugin": False},
                "helperurl": {"exec": True, "plugin": False}}

    monkeypatch.setattr(codex, "_discover_mcp_servers", discover)
    route = {"resolved_model": "fixture"}
    reader = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)
    options = codex_options(reader)
    assert seen and set(seen) == {str(tmp_path)}
    assert "features.hooks=false" in options
    assert reader["applied"]["hooks_disabled"] is True
    assert mcp_table(reader) == {"projsrv": {"enabled": False}, "weird.name": {"enabled": False},
                                 "helperurl": {"enabled": False},
                                 "projurl": {"default_tools_approval_mode": "approve"}}
    assert reader["applied"]["mcp_disabled"] == ["helperurl", "projsrv", "weird.name"]
    assert not any(option.startswith("mcp_servers.") for option in options), "no dotted per-server keys"
    assert "_mcp_inventory" not in reader, "the serialised plan must not carry the inventory"
    assured = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True, intent="assurance")
    assert "features.hooks=false" not in codex_options(assured) and "--ignore-user-config" in assured["argv"]


def test_codex_non_writer_lane_goes_clean_when_mcp_discovery_fails(monkeypatch, tmp_path):
    mod = supervisor()
    monkeypatch.setattr(mod.profile("codex"), "_discover_mcp_servers", lambda cwd: None)
    route = {"resolved_model": "fixture"}
    reader = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)
    assert reader["applied"]["config"] == "clean"
    assert "failed" in reader["applied"]["config_note"]
    assert "--ignore-user-config" in reader["argv"] and "--ignore-rules" in reader["argv"]
    assert "features.hooks=false" not in codex_options(reader)


def test_codex_discovery_treats_codex_errors_as_failure(monkeypatch, tmp_path):
    real = supervisor().profile("codex")
    discover = real._real_discover_mcp_servers  # the autouse stand-in replaced the module attribute
    for outcome in (subprocess.CompletedProcess([], 1, "", "boom"),
                    subprocess.CompletedProcess([], 0, "not json", ""),
                    subprocess.TimeoutExpired("codex", 8),
                    OSError("no codex")):
        def fake(*args, _o=outcome, **kwargs):
            if isinstance(_o, Exception):
                raise _o
            return _o
        monkeypatch.setattr(real.subprocess, "run", fake)
        assert discover(str(tmp_path)) is None
    http = {"type": "streamable_http", "url": "https://x.invalid", "bearer_token_env_var": None,
            "http_headers": None, "env_http_headers": None, "http_headers_helper": None}
    entries = [{"name": "a", "enabled": True, "transport": {"type": "stdio"}},
               {"name": "b", "enabled": True, "transport": http},
               {"name": "c", "enabled": False, "transport": {"type": "stdio"}},
               {"name": "helper", "enabled": True, "transport": {**http, "http_headers_helper": "<redacted>"}},
               {"name": "plug", "enabled": True, "transport": {"type": "stdio"}},
               {"name": "future", "enabled": True, "transport": {**http, "auth_command": "x"}}]
    calls = []

    def listing(argv, **kwargs):
        calls.append((argv, kwargs["timeout"]))
        shown = [e for e in entries if e["name"] != "plug" or "features.plugins=false" not in argv]
        return subprocess.CompletedProcess([], 0, json.dumps(shown), "")

    monkeypatch.setattr(real.subprocess, "run", listing)
    assert discover(str(tmp_path)) == {
        "a": {"exec": True, "plugin": False}, "b": {"exec": False, "plugin": False},
        "helper": {"exec": True, "plugin": False}, "plug": {"exec": True, "plugin": True},
        "future": {"exec": True, "plugin": False}}
    assert len(calls) == 2 and all(timeout <= real.DISCOVERY_SECONDS < 30 for _, timeout in calls)


def test_codex_plugin_servers_use_the_plugin_switch_not_a_server_override(monkeypatch, tmp_path):
    """A plugin server has no config-table transport: `mcp_servers.<name>.enabled=false` makes Codex
    exit with `invalid transport`. Only `features.plugins=false` stops it."""
    mod = supervisor()
    inventory = {"plug": {"exec": True, "plugin": True}, "plugurl": {"exec": False, "plugin": True},
                 "user": {"exec": True, "plugin": False}}
    monkeypatch.setattr(mod.profile("codex"), "_discover_mcp_servers", lambda cwd: inventory)
    route = {"resolved_model": "fixture"}
    reader = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)
    assert "features.plugins=false" in codex_options(reader)
    assert mcp_table(reader) == {"user": {"enabled": False}}
    assert reader["applied"]["plugins_disabled"] is True
    assert reader["applied"]["mcp_disabled"] == ["plug", "user"]
    _, worktree = sandbox_profile_worktree(tmp_path)
    writer = mod.build_plan("codex", route, "hello", mode="worktree_write", worktree=worktree,
                            workspace_root=worktree, network=True)
    assert "features.plugins=false" not in codex_options(writer)
    assert mcp_table(writer) == {"user": {"default_tools_approval_mode": "approve"}}
    assert writer["applied"]["mcp_unconfined"] == ["plug", "user"]
    assert writer["applied"]["mcp_unapproved"] == ["plug", "plugurl"]
    only_urls = {"plugurl": {"exec": False, "plugin": True}}
    monkeypatch.setattr(mod.profile("codex"), "_discover_mcp_servers", lambda cwd: only_urls)
    remote = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)
    assert "features.plugins=false" not in codex_options(remote) and mcp_table(remote) == {}


def test_codex_unspellable_server_name_makes_a_reader_go_clean_and_a_writer_skip_it(monkeypatch, tmp_path):
    mod = supervisor()
    monkeypatch.setattr(mod.profile("codex"), "_discover_mcp_servers",
                        lambda cwd: {"bad\x7fname": {"exec": True, "plugin": False}})
    route = {"resolved_model": "fixture"}
    reader = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)
    assert reader["applied"]["config"] == "clean" and "cannot be overridden" in reader["applied"]["config_note"]
    _, worktree = sandbox_profile_worktree(tmp_path)
    writer = mod.build_plan("codex", route, "hello", mode="worktree_write", worktree=worktree,
                            workspace_root=worktree, network=True)
    assert writer["applied"]["config"] == "inherited" and mcp_table(writer) == {}
    assert writer["applied"]["mcp_unapproved"] == ["bad\x7fname"]


def test_codex_rebuilt_argv_reuses_the_first_config_decision(monkeypatch, tmp_path):
    """A context-ceiling change or effort retry rebuilds argv; discovery must not be re-run or the
    receipt (config, config_note, mcp_disabled) could contradict the argv."""
    mod = supervisor()
    codex = mod.profile("codex")
    answers, calls = iter([None, {"late": {"exec": False, "plugin": False}}]), []
    monkeypatch.setattr(codex, "_discover_mcp_servers", lambda cwd: calls.append(cwd) or next(answers))
    plan = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello", cwd=tmp_path, network=True)
    assert plan["applied"]["config"] == "clean" and "config_note" in plan["applied"]
    first = list(plan["argv"])
    plan["context_ceiling"] = 50000
    rebuilt = codex.argv(plan)
    assert rebuilt[:len(first) - 1].count("--ignore-user-config") == 1
    assert not any(option.startswith("mcp_servers=") for option in rebuilt)
    assert plan["applied"]["config"] == "clean" and "config_note" in plan["applied"]
    assert calls == [str(tmp_path)], "discovery ran only once"


def test_codex_writer_receipt_records_unconfined_hooks_and_stdio_servers(monkeypatch, tmp_path):
    mod = supervisor()
    monkeypatch.setattr(mod.profile("codex"), "_discover_mcp_servers",
                        lambda cwd: {"projsrv": {"exec": True, "plugin": False},
                                     "projurl": {"exec": False, "plugin": False}})
    _, worktree = sandbox_profile_worktree(tmp_path)
    writer = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello", mode="worktree_write",
                            worktree=worktree, workspace_root=worktree, network=True)
    options = codex_options(writer)
    assert "features.hooks=false" not in options
    assert mcp_table(writer)["projsrv"] == {"default_tools_approval_mode": "approve"}
    assert writer["applied"]["hooks_unconfined"] is True
    assert writer["applied"]["mcp_unconfined"] == ["projsrv"]
    assert "hooks_disabled" not in writer["applied"] and "mcp_disabled" not in writer["applied"]


@pytest.mark.live_provider
@pytest.mark.skipif(not shutil.which("codex"), reason="needs a real codex")
def test_live_codex_lists_trusted_project_stdio_server(monkeypatch, tmp_path):
    """Real `codex mcp list --json` sees a server that only a trusted project config defines."""
    codex = supervisor().profile("codex")
    home, project = tmp_path / "home", tmp_path / "proj"
    (project / ".codex").mkdir(parents=True)
    home.mkdir()
    (home / "config.toml").write_text(f'[projects."{project}"]\ntrust_level = "trusted"\n', encoding="utf-8")
    (project / ".codex/config.toml").write_text(
        '[mcp_servers.projsrv]\ncommand = "/bin/sh"\n[mcp_servers."dot.name"]\ncommand = "/bin/sh"\n'
        '[mcp_servers.helper]\nurl = "https://x.invalid/mcp"\nhttp_headers_helper = "/bin/true"\n'
        '[mcp_servers.plain]\nurl = "https://x.invalid/mcp"\n', encoding="utf-8")
    monkeypatch.setenv("CODEX_HOME", str(home))
    inert, runs = {"exec": False, "plugin": False}, {"exec": True, "plugin": False}
    assert codex._real_discover_mcp_servers(str(project)) == {
        "projsrv": runs, "dot.name": runs, "helper": runs, "plain": inert}


def test_codex_native_route_pins_openai_provider_over_user_config(tmp_path):
    mod = supervisor()
    options = codex_options(mod.build_plan("codex", {"resolved_model": "fixture"}, "hello", cwd=tmp_path))
    assert {'model_provider="openai"', 'openai_base_url=""'} <= options
    endpoint = {"resolved_model": "m", "endpoint_base_url": "https://x.example/v1", "endpoint_token_env": "X_KEY"}
    options = codex_options(mod.build_plan("codex", endpoint, "hello", cwd=tmp_path))
    assert 'model_provider="provenant_endpoint"' in options
    assert 'model_provider="openai"' not in options


def test_codex_state_grant_is_skipped_when_it_overlaps_the_workspace(tmp_path, monkeypatch):
    mod = supervisor()
    route = {"resolved_model": "fixture"}
    state = tmp_path / "state"
    monkeypatch.setenv("XDG_STATE_HOME", str(state))
    work = tmp_path / "work"
    work.mkdir()
    plan = mod.build_plan("codex", route, "hello", cwd=work, network=True)
    granted = str((state / "provenant").resolve())
    assert plan["applied"]["write_boundary"]["state_dir"] == granted
    assert granted in " ".join(plan["argv"]) and not (state / "provenant").exists()
    # XDG_STATE_HOME above the workspace: the grant would cover the workspace.
    repo = tmp_path / "projects" / "provenant" / "repo"
    repo.mkdir(parents=True)
    monkeypatch.setenv("XDG_STATE_HOME", str(tmp_path / "projects"))
    plan = mod.build_plan("codex", route, "hello", cwd=repo, network=True)
    boundary = plan["applied"]["write_boundary"]
    assert "state_dir" not in boundary and "overlaps" in boundary["state_dir_skipped"]
    assert str(tmp_path / "projects" / "provenant") not in " ".join(plan["argv"])
    # A symlinked state directory and a glob character are refused too.
    link = tmp_path / "linked"
    (link).mkdir()
    (link / "provenant").symlink_to(repo)
    monkeypatch.setenv("XDG_STATE_HOME", str(link))
    assert "symlink" in mod.build_plan("codex", route, "hello", cwd=work, network=True)[
        "applied"]["write_boundary"]["state_dir_skipped"]
    star = tmp_path / "st*r"
    star.mkdir()
    monkeypatch.setenv("XDG_STATE_HOME", str(star))
    assert "glob" in mod.build_plan("codex", route, "hello", cwd=repo, network=True)[
        "applied"]["write_boundary"]["state_dir_skipped"]


def test_codex_ordinary_lane_inherits_user_config_and_assurance_stays_clean(monkeypatch, tmp_path):
    mod = supervisor()
    home = Path(os.environ["CODEX_HOME"])
    (home / "config.toml").write_text(
        '[mcp_servers.fabric]\ncommand = "x"\n[mcp_servers.computer-use]\ncommand = "y"\n', encoding="utf-8")
    route = {"resolved_model": "fixture"}
    ordinary = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)
    assert ordinary["applied"]["config"] == "inherited"
    assert "--ignore-user-config" not in ordinary["argv"] and "--ignore-rules" not in ordinary["argv"]
    assert {'web_search="live"', "notify=[]", 'approval_policy="never"',
            } <= codex_options(ordinary)
    assert mcp_table(ordinary) == {"fabric": {"enabled": False}, "computer-use": {"enabled": False}}
    # The route and permission overrides come after any user config, so they still win.
    assert any(option.startswith("default_permissions=") for option in codex_options(ordinary))
    assured = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True, intent="assurance")
    assert assured["applied"]["config"] == "clean"
    assert "--ignore-user-config" in assured["argv"] and "--ignore-rules" in assured["argv"]
    assert 'web_search="live"' not in codex_options(assured)
    assert not any("default_tools_approval_mode" in option for option in codex_options(assured))
    monkeypatch.setenv("CF_DISPATCH_CODEX_CONFIG", "clean")
    assert mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)["applied"]["config"] == "clean"
    monkeypatch.setenv("CF_DISPATCH_CODEX_CONFIG", "bogus")
    with pytest.raises(ValueError, match="CF_DISPATCH_CODEX_CONFIG"):
        mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)
    monkeypatch.setenv("CF_DISPATCH_CODEX_CONFIG", "clean")
    capable = mod.build_plan("codex", route, "hello", cwd=tmp_path, network=True)
    assert capable["applied"]["config"] == "clean"


def test_codex_writer_argv_preserves_default_and_capability_sandboxes(monkeypatch, tmp_path):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")

    monkeypatch.setattr(mod.sys, "platform", "linux")
    default_linux = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello",
                                   workspace_root=tmp_path, mode="worktree_write", worktree=lane)
    resumed_linux = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello",
                                   workspace_root=tmp_path, mode="worktree_write", worktree=lane,
                                   resume_session="saved")
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    default_darwin = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello",
                                    workspace_root=tmp_path, mode="worktree_write", worktree=lane)
    resumed_darwin = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello",
                                    workspace_root=tmp_path, mode="worktree_write", worktree=lane,
                                    resume_session="saved")
    def argv(plan):
        # Each plan names its own permissions profile; compare the rest.
        return [arg.replace(plan["applied"]["write_boundary"]["profile"], "profile") for arg in plan["argv"]]

    # An ordinary lane keeps the login shell, so provenant and pnpm sit on PATH as they do natively.
    for before, after in ((argv(default_linux), argv(default_darwin)),
                          (argv(resumed_linux), argv(resumed_darwin))):
        assert after == before
        assert "allow_login_shell=false" not in after
    assured = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
                             mode="worktree_write", worktree=lane, intent="assurance")
    index = argv(assured).index("allow_login_shell=false")
    assert argv(assured)[index - 1] == "-c"

    fresh = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello",
                           workspace_root=tmp_path, mode="worktree_write", worktree=lane,
                           sandbox="workspace-write", network=True, capabilities=["browser"])
    resumed = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello",
                             workspace_root=tmp_path, mode="worktree_write", worktree=lane,
                             sandbox="workspace-write", network=True, capabilities=["browser"],
                             resume_session="saved")
    for plan, sandbox_pair in ((fresh, ["-s", "danger-full-access"]),
                               (resumed, ["-c", 'sandbox_mode="danger-full-access"'])):
        argv = plan["argv"]
        assert any(argv[index:index + 2] == sandbox_pair for index in range(len(argv) - 1))
        assert not any("sandbox_workspace_write." in value for value in argv)
    assert fresh["argv"][fresh["argv"].index("--cd") + 1] == str(lane)
    # codex exec resume rejects --cd; the owner starts it in the worktree.
    assert "--cd" not in resumed["argv"]


@pytest.mark.skipif(sys.platform != "darwin" or not Path("/usr/bin/sandbox-exec").exists(),
                    reason="needs macOS sandbox-exec")
def test_codex_capability_profile_enforces_git_and_signal_limits(monkeypatch, tmp_path):
    mod = supervisor()
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    _, lane = instruction_lane(tmp_path)
    home = (tmp_path / "source-codex-home").resolve()
    home.mkdir()
    (home / "auth.json").write_text("token", encoding="utf-8")
    monkeypatch.setenv("CODEX_HOME", str(home))
    fabric = tmp_path / "fabric-state"
    fabric.mkdir()
    monkeypatch.setenv("AGENT_FABRIC_STATE_DIRECTORY", str(fabric))
    common = Path(git(lane, "rev-parse", "--path-format=absolute", "--git-common-dir").strip())
    private = Path(git(lane, "rev-parse", "--path-format=absolute", "--absolute-git-dir").strip())

    with tempfile.TemporaryDirectory(dir="/tmp") as outside_root, tempfile.TemporaryDirectory(dir="/tmp") as run_root:
        run_dir = Path(run_root) / "a"
        run_dir.mkdir()
        outside_socket = Path(outside_root) / "o"
        inside_socket = run_dir / "i"
        assert len(os.fsencode(outside_socket)) < 104
        assert len(os.fsencode(inside_socket)) < 104
        with (socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as outside_listener,
              socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as inside_listener):
            outside_listener.bind(str(outside_socket))
            outside_listener.listen(1)
            inside_listener.bind(str(inside_socket))
            inside_listener.listen(1)
            plan = mod.build_plan(
                "codex", {}, "hello", workspace_root=tmp_path, mode="worktree_write",
                worktree=lane, sandbox="workspace-write", network=True,
                capabilities=["postgres"], run_dir=run_dir,
            )
            script = f"""
import os
import socket
import sqlite3
def attempt(action):
    try:
        action()
        return "ok"
    except OSError as exc:
        return exc.__class__.__name__
def connect(path):
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        client.connect(path)
    finally:
        client.close()
def write_fabric():
    with sqlite3.connect({str(fabric / "fabric.sqlite3")!r}) as db:
        db.execute("CREATE TABLE notes (body TEXT)")
        assert db.execute("PRAGMA journal_mode=WAL").fetchone() == ("wal",)
        db.execute("INSERT INTO notes VALUES ('lane note')")
print(attempt(write_fabric))
print(attempt(lambda: open({str(lane / "note.txt")!r}, "w").write("x")))
print(attempt(lambda: open({str(lane / ".git")!r}, "a").write("")))
print(attempt(lambda: open({str(private / "config.worktree")!r}, "a").write("")))
print(attempt(lambda: open({str(private / "commondir")!r}, "a").write("")))
print(attempt(lambda: open({str(private / "index.probe")!r}, "w").write("probe")))
print(attempt(lambda: open({str(common / "config")!r}, "a").write("")))
print(attempt(lambda: open({str(common / "hooks" / "pre-commit")!r}, "w").write("")))
print(attempt(lambda: os.link({str(common / "config")!r}, {str(lane / "config-link")!r})))
print(attempt(lambda: open({str(home / "auth.json")!r}, "w").write("refreshed")))
print(attempt(lambda: os.unlink({str(home / "auth.json")!r})))
print(attempt(lambda: os.kill({os.getpid()}, 0)))
print(attempt(lambda: connect({str(outside_socket)!r})))
print(attempt(lambda: connect({str(inside_socket)!r})))
"""
            result = subprocess.run(["/usr/bin/sandbox-exec", "-p", mod.provider_confinement.os_confinement_profile(plan),
                                     sys.executable, "-c", script], capture_output=True, text=True)
            if result.returncode and "sandbox_apply" in result.stderr:
                pytest.skip("sandbox_apply is refused in this test environment")
            assert result.stdout.split() == [
                "ok",
                "ok", "PermissionError", "PermissionError", "PermissionError", "ok",
                "PermissionError", "PermissionError", "PermissionError", "ok", "PermissionError",
                "PermissionError", "PermissionError", "ok",
            ], result.stderr
            assert not (lane / "config-link").exists()
            assert (home / "auth.json").read_text(encoding="utf-8") == "refreshed"
            assert (private / "index.probe").read_text(encoding="utf-8") == "probe"
            import sqlite3
            with sqlite3.connect(fabric / "fabric.sqlite3") as db:
                assert db.execute("SELECT body FROM notes").fetchall() == [("lane note",)]


@pytest.mark.skipif(sys.platform != "darwin" or not Path("/usr/bin/sandbox-exec").exists(),
                    reason="needs macOS sandbox-exec")
def test_a_linked_git_entry_never_moves_the_writable_git_grant(monkeypatch, tmp_path):
    mod = supervisor()
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    _, lane = instruction_lane(tmp_path)
    home = (tmp_path / "source-codex-home").resolve()
    home.mkdir()
    monkeypatch.setenv("CODEX_HOME", str(home))
    common = Path(git(lane, "rev-parse", "--path-format=absolute", "--git-common-dir").strip()).resolve()
    outside = (tmp_path / "outside").resolve()
    outside.mkdir()
    run_dir = tmp_path / "runs/task/attempt-001"
    run_dir.mkdir(parents=True)

    def plan_lane():
        return mod.build_plan(
            "codex", {}, "hello", workspace_root=tmp_path, mode="worktree_write",
            worktree=lane, sandbox="workspace-write", network=True,
            capabilities=["postgres"], run_dir=run_dir,
        )

    plan = plan_lane()
    shutil.rmtree(common / "logs", ignore_errors=True)
    (common / "logs").symlink_to(outside, target_is_directory=True)
    (common / "packed-refs").unlink(missing_ok=True)
    (common / "packed-refs").symlink_to(outside / "packed")
    # Planning never creates the writer's ref directories through a link.
    with pytest.raises(ValueError, match="is a link"):
        plan_lane()
    profile = mod.provider_confinement.os_confinement_profile(plan)
    assert str(outside) not in profile
    assert f'(literal "{common / "logs/refs/heads/lane"}")' in profile
    script = f"""
for path in ({str(common / "logs" / "probe")!r}, {str(common / "packed-refs")!r}):
    try:
        open(path, "w").write("x")
        print("ok")
    except OSError as exc:
        print(exc.__class__.__name__)
"""
    result = subprocess.run(["/usr/bin/sandbox-exec", "-p", profile, sys.executable, "-c", script],
                            capture_output=True, text=True)
    if result.returncode and "sandbox_apply" in result.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert result.stdout.split() == ["PermissionError", "PermissionError"], result.stderr
    assert list(outside.iterdir()) == []


@pytest.mark.parametrize(("capabilities", "browser_tmp"), [(["postgres"], None), (["browser"], "tmp")])
def test_codex_capability_home_is_seeded_and_browser_tmp_is_opt_in(
    monkeypatch, tmp_path, capabilities, browser_tmp,
):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    monkeypatch.chdir(tmp_path)
    attempt = tmp_path / "runs/task/attempt-001"
    attempt.mkdir(parents=True)
    source = tmp_path / "source-codex-home"
    (source / "skills").mkdir(parents=True)
    (source / "auth.json").write_text("auth", encoding="utf-8")
    (source / "AGENTS.md").write_text("instructions", encoding="utf-8")
    (source / "HARNESS.md").write_text("constitution", encoding="utf-8")
    (source / "skills/example").mkdir()
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    monkeypatch.setenv("CODEX_HOME", "source-codex-home")
    plan = mod.build_plan("codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
                          mode="worktree_write", worktree=lane, sandbox="workspace-write",
                          network=True, capabilities=capabilities, run_dir=attempt)
    plan["applied"]["confinement"] = "none"
    code = "import json, os; print(json.dumps({'type':'result','result':json.dumps({k:os.environ.get(k) for k in ('CODEX_HOME','MAC_CHROMIUM_TMPDIR','TMPDIR','XDG_CACHE_HOME')})}))"
    plan["argv"] = [sys.executable, "-u", "-c", code]

    environment = {**os.environ, "CODEX_HOME": "source-codex-home", "MAC_CHROMIUM_TMPDIR": "ambient-value"}
    record = mod.execute(plan, attempt / "result.md", env=environment)

    lane_home = attempt.parent / "codex-home"
    assert record["status"] == "ok"
    assert json.loads((attempt / "result.md").read_text()) == {
        "CODEX_HOME": str(lane_home),
        "MAC_CHROMIUM_TMPDIR": plan["applied"]["browser_tmpdir"] if browser_tmp else None,
        "TMPDIR": str(attempt / "tmp"),
        "XDG_CACHE_HOME": str(attempt / "tmp/cache"),
    }
    if browser_tmp:
        assert not Path(record["applied"]["browser_tmpdir"]).exists()
    else:
        assert "browser_tmpdir" not in record["applied"]
    for name in ("auth.json", "AGENTS.md", "HARNESS.md", "skills"):
        assert (lane_home / name).is_symlink()
        assert (lane_home / name).resolve() == (source / name).resolve()

    resumed_attempt = attempt.parent / "attempt-002"
    resumed_attempt.mkdir()
    resumed_plan = mod.build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
        mode="worktree_write", worktree=lane, sandbox="workspace-write", network=True,
        capabilities=capabilities, run_dir=resumed_attempt,
    )
    another_source = tmp_path / "another-codex-home"
    another_source.mkdir()
    other_auth = another_source / "auth.json"
    other_auth.write_text("untrusted auth", encoding="utf-8")
    (lane_home / "auth.json").unlink()
    (lane_home / "auth.json").symlink_to(other_auth)
    (lane_home / "AGENTS.md").unlink()
    (lane_home / "AGENTS.md").write_text("lane-controlled instructions", encoding="utf-8")
    resumed_environment = {"CODEX_HOME": str(source)}
    mod._prepare_codex_capability_home(resumed_plan, resumed_environment)
    assert resumed_environment["CODEX_HOME"] == str(lane_home)
    auth_path = (source / "auth.json").resolve()
    assert (lane_home / "auth.json").resolve() == auth_path
    assert (lane_home / "AGENTS.md").is_symlink()
    assert (lane_home / "AGENTS.md").resolve() == (source / "AGENTS.md").resolve()
    assert resumed_plan["codex_auth_path"] == str(auth_path)
    resumed_profile = mod.provider_confinement.os_confinement_profile(resumed_plan)
    assert f'(literal "{auth_path}")' in resumed_profile
    untrusted_paths = {str(other_auth), str(other_auth.resolve())}
    assert not (untrusted_paths & {resumed_plan["codex_auth_path"]})
    assert not any(path in str(resumed_plan["applied"]["write_boundary"]) for path in untrusted_paths)
    assert not any(path in resumed_profile for path in untrusted_paths)


def test_codex_capability_home_symlink_fails_before_provider_launch(monkeypatch, tmp_path):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    source = tmp_path / "source-codex-home"
    source.mkdir()
    (source / "auth.json").write_text("auth", encoding="utf-8")
    task = tmp_path / "runs/task"
    attempt = task / "attempt-001"
    attempt.mkdir(parents=True)
    outside_home = tmp_path / "outside-codex-home"
    outside_home.mkdir()
    lane_home = task / "codex-home"
    lane_home.symlink_to(outside_home, target_is_directory=True)
    launched = tmp_path / "provider-launched"
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    monkeypatch.setenv("CODEX_HOME", str(source))
    plan = mod.build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
        mode="worktree_write", worktree=lane, sandbox="workspace-write", network=True,
        capabilities=["postgres"], run_dir=attempt,
    )
    plan["applied"]["confinement"] = "none"
    plan["argv"] = [
        sys.executable, "-u", "-c",
        "from pathlib import Path; import json; "
        f"Path({str(launched)!r}).write_text('started'); "
        "print(json.dumps({'type':'result','result':'started'}))",
    ]

    record = mod.execute(plan, attempt / "result.md", env={**os.environ, "CODEX_HOME": str(source)})

    expected_error = f"codex capability home is not a directory: {lane_home}"
    assert record["status"] == "failed"
    assert expected_error in record["reason"]
    assert expected_error in record["evidence"]["excerpt"]
    assert not launched.exists()


def test_codex_capability_symlinked_source_auth_fails_and_never_widens_the_grant(monkeypatch, tmp_path):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    source = (tmp_path / "source-codex-home").resolve()
    source.mkdir()
    (source / "config.toml").write_text("model = 'x'", encoding="utf-8")
    (source / "auth.json").symlink_to(source / "config.toml")
    attempt = tmp_path / "runs/task/attempt-001"
    attempt.mkdir(parents=True)
    launched = tmp_path / "provider-launched"
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    monkeypatch.setenv("CODEX_HOME", str(source))
    plan = mod.build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
        mode="worktree_write", worktree=lane, sandbox="workspace-write", network=True,
        capabilities=["postgres"], run_dir=attempt,
    )
    profile = mod.provider_confinement.os_confinement_profile(plan)
    assert f'(allow file-write* (literal "{source / "auth.json"}"))' in profile
    assert f'(deny file-write-create file-write-unlink (literal "{source / "auth.json"}"))' in profile
    assert str(source / "config.toml") not in profile
    plan["applied"]["confinement"] = "none"
    plan["argv"] = [
        sys.executable, "-u", "-c",
        f"from pathlib import Path; Path({str(launched)!r}).write_text('started')",
    ]

    record = mod.execute(plan, attempt / "result.md", env={**os.environ, "CODEX_HOME": str(source)})

    assert record["status"] == "failed"
    assert f"codex auth store is not a regular file: {source / 'auth.json'}" in record["reason"]
    assert not launched.exists()


@pytest.mark.parametrize("placement", ["inside-lane", "symlink-alias"])
def test_codex_capability_swappable_source_home_fails_before_launch(monkeypatch, tmp_path, placement):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    home = (tmp_path / "real-codex-home").resolve()
    home.mkdir()
    (home / "auth.json").write_text("{}", encoding="utf-8")
    if placement == "inside-lane":
        source = (lane / "codex-source").resolve()
        home.rename(source)
        reason = f"codex home lies inside a lane-writable path: {source}"
    else:
        source = lane / "codex-alias"
        source.symlink_to(home, target_is_directory=True)
        reason = f"codex home must not pass through a symlink: {source}"
    attempt = tmp_path / "runs/task/attempt-001"
    attempt.mkdir(parents=True)
    launched = tmp_path / "provider-launched"
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    monkeypatch.setenv("CODEX_HOME", str(source))
    plan = mod.build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
        mode="worktree_write", worktree=lane, sandbox="workspace-write", network=True,
        capabilities=["browser"], run_dir=attempt,
    )
    plan["applied"]["confinement"] = "none"
    plan["argv"] = [
        sys.executable, "-u", "-c",
        f"from pathlib import Path; Path({str(launched)!r}).write_text('started')",
    ]

    record = mod.execute(plan, attempt / "result.md", env={**os.environ, "CODEX_HOME": str(source)})

    assert record["status"] == "failed"
    assert reason in record["reason"]
    assert not launched.exists()
    assert not Path(record["applied"]["browser_tmpdir"]).exists()


def test_browser_temporary_directory_refuses_a_planted_link_before_launch(monkeypatch, tmp_path):
    mod = supervisor()
    plan = mod.build_plan("claude", {}, "hello", cwd=tmp_path, workspace_root=tmp_path,
                          capabilities=["browser"], run_dir=tmp_path / "attempt")
    temporary = Path(plan["applied"]["browser_tmpdir"])
    victim = tmp_path / "victim"
    victim.mkdir()
    marker = victim / "keep"
    marker.write_text("preserved")
    temporary.symlink_to(victim, target_is_directory=True)
    launched = tmp_path / "launched"
    plan["argv"] = [sys.executable, "-c", f"open({str(launched)!r}, 'w').close()"]
    try:
        record = mod.execute(plan, tmp_path / "result.md")
        assert record["status"] == "failed"
        assert not launched.exists()
        assert temporary.is_symlink()
        assert marker.read_text() == "preserved"
    finally:
        temporary.unlink()


def test_browser_temporary_directory_is_removed_when_a_callback_raises(monkeypatch, tmp_path):
    mod = supervisor()
    force_sandbox(monkeypatch, None)
    plan = mod.build_plan("claude", {}, "hello", cwd=tmp_path, workspace_root=tmp_path,
                          capabilities=["browser"], run_dir=tmp_path / "attempt")
    plan["argv"] = [sys.executable, "-c", "import time; time.sleep(30)"]
    temporary = Path(plan["applied"]["browser_tmpdir"])

    def started(process):
        assert temporary.is_dir()
        (temporary / "browser-state").write_text("temporary")
        raise RuntimeError("start callback failed")

    with pytest.raises(RuntimeError, match="start callback failed"):
        mod.execute(plan, tmp_path / "result.md", on_start=started)
    assert not temporary.exists()


def test_claude_adapter_receives_attempt_private_claude_tmpdir(tmp_path):
    code = """import json, os
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':json.dumps({key: os.environ.get(key) for key in ('TMPDIR','CLAUDE_TMPDIR','CLAUDE_CODE_TMPDIR')})}}))
print(json.dumps({'type':'turn.completed'}))
"""
    plan = fixture_plan(tmp_path, code, adapter="claude")
    # A directory left by an earlier attempt is tightened, not trusted.
    (tmp_path / "tmp/claude").mkdir(parents=True)
    (tmp_path / "tmp").chmod(0o755)
    (tmp_path / "tmp/claude").chmod(0o755)
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    paths = json.loads((tmp_path / "result.md").read_text())
    assert paths["TMPDIR"] == str(tmp_path / "tmp")
    assert paths["CLAUDE_TMPDIR"] == str(tmp_path / "tmp/claude")
    # Claude Code otherwise opens /tmp/claude-<uid>, which a read-only profile cannot read.
    assert paths["CLAUDE_CODE_TMPDIR"] == str(tmp_path / "tmp/claude")
    assert (tmp_path / "tmp/claude").is_dir()
    # Private to the provider's user, as the Codex lane home is.
    assert (tmp_path / "tmp").stat().st_mode & 0o777 == 0o700
    assert (tmp_path / "tmp/claude").stat().st_mode & 0o777 == 0o700


def test_codex_read_only_records_native_write_boundary(tmp_path):
    plan = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello",
                                   cwd=tmp_path, workspace_root=tmp_path)
    assert plan["applied"]["confinement"] == "provider-native"
    assert plan["applied"]["write_boundary"]["kind"] == "provider-native"


def test_worker_preface_explains_process_cleanup(tmp_path):
    plan = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello", cwd=tmp_path, workspace_root=tmp_path)
    assert "processes left running are then stopped." in plan["prompt"]


def test_provider_launch_sets_pwd_to_resolved_cwd(tmp_path):
    child = tmp_path / "child"
    child.mkdir()
    code = "import json, os; print(json.dumps({'type':'result','result':os.environ['PWD']}))"
    plan = fixture_plan(tmp_path, code)
    plan["cwd"] = str(child)
    record = supervisor().execute(
        plan,
        tmp_path / "result.md",
        env={**os.environ, "PWD": str(tmp_path)},
    )
    assert record["status"] == "ok"
    assert record["output_path"]
    assert (tmp_path / "result.md").read_text() == str(child)


def test_opencode_read_only_declares_fully_denied_bash_tool(tmp_path):
    code = "import json, os; print(json.dumps({'type':'result','result':os.environ['OPENCODE_CONFIG_CONTENT']}))"
    record = supervisor().execute(
        fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md"
    )
    permission = json.loads((tmp_path / "result.md").read_text())["permission"]
    assert permission["bash"]["*"] == "deny"
    assert permission["bash"]["provenant-no-shell"] == "allow"


def test_opencode_run_disables_foreign_instruction_discovery(tmp_path):
    code = "import json, os; print(json.dumps({'type':'result','result':os.environ.get('OPENCODE_DISABLE_CLAUDE_CODE','') + os.environ.get('OPENCODE_DISABLE_AUTOUPDATE','')}))"
    supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert (tmp_path / "result.md").read_text() == "11"


@pytest.mark.parametrize("confined", [False, True])
def test_opencode_disables_fabric_mcp_only_in_sandboxed_lanes(tmp_path, monkeypatch, confined):
    code = "import os, json; print(json.dumps({'type':'result','result':os.environ['OPENCODE_CONFIG_CONTENT']}))"
    plan = fixture_plan(tmp_path, code, "opencode")
    if confined:
        launcher = tmp_path / "sandbox-exec"
        launcher.write_text('#!/bin/sh\nshift 2\nexec "$@"\n')
        launcher.chmod(0o755)
        force_sandbox(monkeypatch, str(launcher))
        plan["applied"]["confinement"] = "sandbox-exec"
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    config = json.loads((tmp_path / "result.md").read_text())
    assert config.get("mcp") == ({"servers": {"fabric": {"type": "local", "command": ["provenant"], "disabled": True}}}
                                 if confined else None)
    assert config["permission"]["edit"]["*"] == "deny"


@pytest.mark.parametrize("sandboxed", [False, True], ids=["shim", "sandbox-exec"])
@pytest.mark.parametrize("failure", [None, "missing", "denied", "bad_handshake", "timeout", "sandbox_missing"])
def test_opencode_writer_preflights_the_configured_fabric_command(tmp_path, monkeypatch, failure, sandboxed):
    mod = supervisor()
    native_sandbox = mod.provider_confinement._sandbox_exec_path() if sandboxed else None
    if sandboxed and not native_sandbox:
        pytest.skip("sandbox-exec unavailable or nested sandbox refused")
    adapter = mod.profile("opencode")
    product = tmp_path / "product"
    wrapper = product / "runtime/fabric/bin/fabric-mcp"
    wrapper.parent.mkdir(parents=True)
    wrapper.write_text(
        "#!" + sys.executable + "\nimport json, os, sys, time\nassert os.environ.get('AGENT_FABRIC_SEAT') == 'opencode'\n"
        + ("time.sleep(5)\n" if failure == "timeout" else "")
        + "request = json.loads(sys.stdin.readline())\n"
        + ("print('tsx loader not found', file=sys.stderr)\n" if failure == "bad_handshake" else "")
        + "print(json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': "
        + ("{}" if failure == "bad_handshake" else
           "{'protocolVersion': '2024-11-05', 'capabilities': {}, 'serverInfo': {'name': 'fabric', 'version': 'test'}}")
        + "}), flush=True)\n"
    )
    wrapper.chmod(0o644 if failure == "denied" else 0o755)
    if failure == "missing":
        wrapper.unlink()
    monkeypatch.setattr(adapter, "FABRIC_MCP", wrapper, raising=False)
    monkeypatch.setattr(mod, "MCP_PREFLIGHT_SECONDS", 0.5, raising=False)
    provider_started = tmp_path / "provider-started"
    code = ("import json; from pathlib import Path; Path(" + repr(str(provider_started))
            + ").touch(); print(json.dumps({'type':'result','result':'done'}))")
    plan = fixture_plan(tmp_path, code, "opencode")
    plan["mode"] = "worktree_write"
    # Model and MCP processes both go through this same sandbox boundary.
    sandbox_log = tmp_path / "sandbox-commands"
    sandbox = tmp_path / "sandbox-exec"
    sandbox.write_text('#!/bin/sh\nprintf "%s\\n" "$3" >> ' + shlex.quote(str(sandbox_log))
                       + '\nshift 2\nexec "$@"\n')
    sandbox.chmod(0o755)
    force_sandbox(monkeypatch, None if failure == "sandbox_missing" else native_sandbox or str(sandbox))
    plan["applied"]["confinement"] = "sandbox-exec"
    record = mod.execute(plan, tmp_path / "result.md")
    config = json.loads(adapter.environment(plan)["OPENCODE_CONFIG_CONTENT"])
    fabric = config["mcp"]["servers"]["fabric"]
    assert fabric["command"] == [str(wrapper)]
    assert fabric["disabled"] is False
    assert fabric["environment"] == {"AGENT_FABRIC_SEAT": "opencode"}
    assert record["status"] == ("rejected" if failure else "ok")
    assert provider_started.exists() is (failure is None)
    assert record["error_class"] == ("fabric_mcp_unavailable" if failure else "none")
    if not sandboxed and failure != "sandbox_missing":
        assert sandbox_log.read_text().splitlines()[0] == str(wrapper)
    if failure:
        assert record["error"] == "fabric_mcp_unavailable"
        assert record["retryable"] is False
        assert "Node" in record["fix"]
    if failure == "bad_handshake":
        assert "tsx loader not found" in json.dumps(record)


@pytest.mark.parametrize("shape", ["stderr", "long_stderr", "structured"])
def test_opencode_free_tier_refusal_is_model_unavailable(tmp_path, shape):
    message = "403 FreeTierError: OpenCode's free tier can only be used from within OpenCode"
    if shape != "stderr":
        message = "diagnostic preamble " * 20 + message
    if shape == "structured":
        code = f"import sys; print({json.dumps({'type': 'error', 'error': message})!r}); sys.exit(1)"
    else:
        code = f"import sys; print({message!r}, file=sys.stderr); sys.exit(1)"
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == "model_unavailable"
    assert "free-tier request" in record["fix"]
    assert "paid opencode-go model" in record["fix"]


@pytest.mark.skipif(sys.platform != "darwin", reason="Codex seatbelt is macOS-only")
def test_codex_seatbelt_provider_finds_ps_shim_on_path(tmp_path):
    observed = tmp_path / "ps-path.txt"
    code = f"""import json, os, pathlib, shutil
pathlib.Path({str(observed)!r}).write_text(shutil.which('ps') or '')
print(json.dumps({{'type':'turn.completed'}}), flush=True)
"""
    plan = fixture_plan(tmp_path, code)
    supervisor().execute(plan, tmp_path / "result.md")
    assert observed.read_text() == str(tmp_path / "tmp/provenant-shim/bin/ps")


def test_unknown_cpu_census_is_not_zero_cpu_progress(monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod.process_info, "processes",
                        lambda: (_ for _ in ()).throw(OSError("census unavailable")))
    assert mod._cpu_stamp(os.getpid()) is None


@pytest.fixture
def provider_clock(monkeypatch):
    offset = [0.0]
    monkeypatch.setattr(supervisor(), "time", SimpleNamespace(
        monotonic=lambda: time.monotonic() + offset[0], sleep=time.sleep, time_ns=time.time_ns))

    def advance(seconds):
        offset[0] += seconds

    return advance


def _child_census_barrier(monkeypatch, pid_path, ready):
    """Let the fixture exit only after its detached child is really tracked."""
    original = supervisor()._Descendants.sample
    trackers = []

    def sampled(self, *args, **kwargs):
        trackers[:] = [self]
        rows = original(self, *args, **kwargs)
        if pid_path.exists() and any(pid == int(pid_path.read_text()) for pid, _ in self.tracked):
            ready.touch()
        return rows

    monkeypatch.setattr(supervisor()._Descendants, "sample", sampled)

    def started(_process):
        deadline = time.monotonic() + 5
        while not pid_path.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        assert pid_path.exists(), "provider did not publish its child PID"
        trackers[0].sample()
        assert ready.exists(), "provider child was not tracked"

    return started


@pytest.mark.parametrize("stop", ["cancelled", "timed_out", "normal"])
def test_new_session_grandchild_does_not_outlive_attempt(tmp_path, monkeypatch, stop):
    pid_path = tmp_path / "grandchild.pid"
    ready = tmp_path / "child-tracked"
    on_start = _child_census_barrier(monkeypatch, pid_path, ready)
    code = f"""import json, pathlib, subprocess, sys, time
child = subprocess.Popen(
    [sys.executable, '-c', 'import time; time.sleep(60)'],
    start_new_session=True,
    stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
)
pathlib.Path({str(pid_path)!r}).write_text(str(child.pid))
if {stop!r} == 'normal':
    while not pathlib.Path({str(ready)!r}).exists():
        time.sleep(0.01)
    print(json.dumps({{'type': 'item.completed', 'item': {{'type': 'agent_message', 'text': 'DONE'}}}}), flush=True)
    print(json.dumps({{'type': 'turn.completed'}}), flush=True)
else:
    time.sleep(60)
"""
    plan = fixture_plan(
        tmp_path,
        code,
        timeout_seconds=3 if stop == "timed_out" else 8,
    )
    if stop == "normal":
        plan["warnings"].append("route warning " + "x" * 240)
    unrelated = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(60)"],
        start_new_session=True,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        record = supervisor().execute(
            plan,
            tmp_path / "result.md",
            on_start=on_start,
            cancelled=lambda: stop == "cancelled"
            and ready.exists(),
        )
        assert record["status"] == ("ok" if stop == "normal" else stop)
        assert pid_path.exists()
        pid = int(pid_path.read_text())
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(0.02)
        with pytest.raises(ProcessLookupError):
            os.kill(pid, 0)
        os.kill(unrelated.pid, 0)
        if stop == "normal":
            assert any(item["pid"] == pid for item in record["reaped"])
            assert record["warnings"][0] == "reaped 1 leftover process(es)"
    finally:
        if pid_path.exists():
            try:
                os.killpg(int(pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass
        if unrelated.poll() is None:
            os.killpg(unrelated.pid, signal.SIGKILL)
        unrelated.wait(timeout=3)


def test_unobserved_double_fork_is_not_reaped_on_marker_alone(tmp_path):
    pid_path = tmp_path / "double-fork.pid"
    grandchild = f"""import os,pathlib,time
pid = os.fork()
if pid:
    os._exit(0)
os.setsid()
pathlib.Path({str(pid_path)!r}).write_text(str(os.getpid()))
time.sleep(60)
"""
    code = f"""import json,subprocess,sys,time
launcher = subprocess.Popen([sys.executable, '-c', {grandchild!r}],
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
launcher.wait()
time.sleep(.1)
print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'DONE'}}}}), flush=True)
print(json.dumps({{'type':'turn.completed'}}), flush=True)
"""
    plan = fixture_plan(tmp_path, code, timeout_seconds=8)
    try:
        record = supervisor().execute(plan, tmp_path / "result.md")
        assert record["status"] == "ok"
        assert pid_path.exists()
        pid = int(pid_path.read_text())
        assert _live_process(pid)
        assert not any(row["pid"] == pid for row in record["reaped"])
    finally:
        if pid_path.exists():
            try:
                os.killpg(int(pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass


def started_sleeper(argv, environment):
    """A sleeping Python child that has finished exec and runs user code.

    Popen returns once exec has closed its error pipe, which on Linux is before the kernel sets
    the new image's environment bounds, so /proc/<pid>/environ can still read empty. The child's
    first output line proves exec is complete; the marker scan only ever sees such processes on
    later censuses.
    """
    child = subprocess.Popen(argv, env=environment, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    ready = child.stdout.readline()
    child.stdout.close()  # the child writes nothing more
    assert ready == b"ready\n"
    return child


def test_attempt_marker_matches_inherited_environment_only():
    marker = "fixture-marker-123"
    sleeper = "import sys, time; print('ready', flush=True); time.sleep(30)"
    environment = dict(os.environ)
    environment.pop("PROVENANT_ATTEMPT_MARKER", None)
    child = started_sleeper([sys.executable, "-c", sleeper, "PROVENANT_ATTEMPT_MARKER=" + marker], environment)
    try:
        assert not supervisor()._has_attempt_marker(child.pid, marker)
    finally:
        child.terminate()
        child.wait(timeout=3)
    environment["PROVENANT_ATTEMPT_MARKER"] = marker
    child = started_sleeper([sys.executable, "-c", sleeper], environment)
    try:
        assert supervisor()._has_attempt_marker(child.pid, marker)
    finally:
        child.terminate()
        child.wait(timeout=3)

def _live_process(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    if sys.platform.startswith("linux"):
        stat = Path(f"/proc/{pid}/stat")
        if stat.exists() and stat.read_text().rsplit(") ", 1)[-1].startswith("Z"):
            return False
    return True


def test_snapshot_exception_still_kills_provider_and_records_attempt(tmp_path, monkeypatch):
    module = supervisor()
    pid_path = tmp_path / "provider.pid"
    code = f"import os,pathlib,time; pathlib.Path({str(pid_path)!r}).write_text(str(os.getpid())); time.sleep(60)"
    plan = fixture_plan(tmp_path, code, timeout_seconds=8)
    monkeypatch.setattr(module, "_process_snapshot", lambda: (_ for _ in ()).throw(RuntimeError("census failed")))
    try:
        record = module.execute(
            plan, tmp_path / "result.md", cancelled=lambda: pid_path.exists()
        )
        assert record["status"] == "cancelled"
        assert any("census unavailable" in warning for warning in record["warnings"])
        assert not _live_process(int(pid_path.read_text()))
    finally:
        if pid_path.exists():
            try:
                os.killpg(int(pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass


def test_internal_census_failure_warns_and_kills_provider(tmp_path, monkeypatch):
    module = supervisor()
    pid_path = tmp_path / "provider.pid"
    code = f"import os,pathlib,time; pathlib.Path({str(pid_path)!r}).write_text(str(os.getpid())); time.sleep(60)"
    monkeypatch.setattr(module, "_process_snapshot_unchecked",
                        lambda: (_ for _ in ()).throw(RuntimeError("census failed")))
    try:
        record = module.execute(
            fixture_plan(tmp_path, code, timeout_seconds=8), tmp_path / "result.md",
            cancelled=lambda: pid_path.exists(),
        )
        assert record["status"] == "cancelled"
        assert any("census unavailable" in warning for warning in record["warnings"])
        assert not _live_process(int(pid_path.read_text()))
    finally:
        if pid_path.exists():
            try:
                os.killpg(int(pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass


def test_linux_snapshot_replaces_non_utf8_process_names(tmp_path, monkeypatch):
    module = supervisor()
    process_dir = tmp_path / "123"
    process_dir.mkdir()
    (process_dir / "stat").write_bytes(
        b"123 (bad\xffname) S 1 123 " + b"0 " * 16 + b"12345 0\n"
    )
    real_scandir = os.scandir
    with monkeypatch.context() as patch:
        patch.setattr(module.sys, "platform", "linux")
        patch.setattr(module.os, "scandir", lambda _path: real_scandir(tmp_path))
        rows = module._process_snapshot()
    assert rows[123].command == "bad\ufffdname"
    assert rows[123].started == "12345"


def test_darwin_libproc_is_loaded_once(monkeypatch):
    module = supervisor()
    loads = []

    class Call:
        def __call__(self, *_args):
            return 0

    class Library:
        proc_listallpids = Call()
        proc_pidinfo = Call()

    with monkeypatch.context() as patch:
        module._darwin_libproc.cache_clear()
        patch.setattr(module.sys, "platform", "darwin")
        patch.setattr(module.ctypes.util, "find_library", lambda _name: "libproc")
        patch.setattr(module.ctypes, "CDLL", lambda *_args, **_kwargs: loads.append(1) or Library())
        module._process_snapshot()
        module._process_snapshot()
        module._darwin_libproc.cache_clear()
    assert len(loads) == 1


def test_linux_tree_census_walks_task_children(tmp_path):
    module = supervisor()
    for pid, ppid, children in ((101, 1, "102 103"), (102, 101, "104"),
                                (103, 101, ""), (104, 102, "")):
        process = tmp_path / str(pid)
        task = process / "task" / str(pid)
        task.mkdir(parents=True)
        (task / "children").write_text(children)
        (process / "stat").write_text(
            f"{pid} (fixture) S {ppid} {pid} " + "0 " * 16 + f"{pid * 100} 0\n"
        )
    rows = module._linux_tree_snapshot(101, set(), proc_root=tmp_path)
    assert set(rows) == {101, 102, 103, 104}


def test_linux_targeted_tree_census_does_not_warn_without_owner_row(monkeypatch):
    module = supervisor()
    process = type("Process", (), {"pid": 123, "poll": lambda self: None})()
    root = module._ProcessRow(123, 1, 123, "12345", "fixture")
    with monkeypatch.context() as patch:
        patch.setattr(module.sys, "platform", "linux")
        patch.setattr(module, "_linux_tree_snapshot", lambda *_args, **_kwargs: {123: root})
        tracker = module._Descendants(process, "marker")
        tracker.sample()
    assert tracker.snapshot_unavailable is False


def test_watchdog_census_runs_about_once_per_second(tmp_path, monkeypatch, provider_clock):
    module = supervisor()
    ready = tmp_path / "census-complete"
    counts = []
    stopping = []
    original_sample = module._Descendants.sample
    original_stop = module._Descendants.stop

    def counted(self, *args, **kwargs):
        if not stopping:
            counts.append(module.time.monotonic())
            if len(counts) == 2:
                ready.touch()
        return original_sample(self, *args, **kwargs)

    def stopped(self, *args, **kwargs):
        stopping.append(True)
        return original_stop(self, *args, **kwargs)

    monkeypatch.setattr(module._Descendants, "sample", counted)
    monkeypatch.setattr(module._Descendants, "stop", stopped)
    code = f"""import pathlib,time
print('ready', flush=True)
while not pathlib.Path({str(ready)!r}).exists():
    time.sleep(0.01)
"""
    plan = fixture_plan(tmp_path, code, timeout_seconds=5)
    module.execute(plan, tmp_path / "result.md", on_progress=lambda _: provider_clock(1.35))
    # A slow host may census again before the provider sees the ready file.
    assert len(counts) >= 2
    assert all(later - earlier >= 1 for earlier, later in zip(counts, counts[1:]))


def test_missing_census_still_kills_provider_group_after_leader_exit(tmp_path, monkeypatch):
    module = supervisor()
    child_pid = tmp_path / "child.pid"
    code = f"""import json,pathlib,subprocess
child = subprocess.Popen(['sleep', '60'], stdin=subprocess.DEVNULL,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path({str(child_pid)!r}).write_text(str(child.pid))
print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'DONE'}}}}), flush=True)
print(json.dumps({{'type':'turn.completed'}}), flush=True)
"""
    plan = fixture_plan(tmp_path, code, timeout_seconds=8)
    monkeypatch.setattr(module, "_process_snapshot", lambda: {})
    leader = []
    try:
        record = module.execute(plan, tmp_path / "result.md", on_start=lambda process: leader.append(process.pid))
        assert record["status"] == "ok"
        assert child_pid.exists()
        pid = int(child_pid.read_text())
        deadline = time.monotonic() + 2
        while _live_process(pid) and time.monotonic() < deadline:
            time.sleep(0.02)
        assert not _live_process(pid)
    finally:
        if leader:
            try:
                os.killpg(leader[0], signal.SIGKILL)
            except ProcessLookupError:
                pass
        if sys.platform.startswith("linux") and child_pid.exists():
            try:
                os.waitpid(int(child_pid.read_text()), os.WNOHANG)
            except ChildProcessError:
                pass


@pytest.mark.parametrize("terminal_event", [False, True])
def test_cleanly_exiting_child_is_not_reported_as_reaped(tmp_path, terminal_event):
    pid_path = tmp_path / "closing-child.pid"
    code = f"""import json,pathlib,subprocess,sys,time
child = subprocess.Popen(
    [sys.executable, '-c', 'import sys,time; sys.stdin.read(); time.sleep(.3)'],
    start_new_session=True, stdin=subprocess.PIPE,
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path({str(pid_path)!r}).write_text(str(child.pid))
print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'DONE'}}}}), flush=True)
print(json.dumps({{'type':'turn.completed'}}), flush=True)
if {terminal_event!r}:
    child.stdin.close()
    time.sleep(30)
"""
    plan = fixture_plan(tmp_path, code, timeout_seconds=8)
    try:
        record = supervisor().execute(plan, tmp_path / "result.md")
        assert record["status"] == "ok"
        assert record["reaped"] == []
        assert not any("reaped " in warning for warning in record["warnings"])
    finally:
        if pid_path.exists():
            try:
                os.killpg(int(pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass


def test_root_provider_keeps_two_second_sigterm_flush_grace(tmp_path):
    code = """import pathlib,signal,time
def stop(*_args):
    time.sleep(1)
    pathlib.Path('flushed').write_text('saved')
    raise SystemExit(0)
signal.signal(signal.SIGTERM, stop)
pathlib.Path('ready').touch()
time.sleep(30)
"""
    plan = fixture_plan(tmp_path, code, timeout_seconds=8)
    root = []
    try:
        record = supervisor().execute(
            plan, tmp_path / "result.md",
            on_start=lambda process: root.append(process.pid),
            cancelled=lambda: (tmp_path / "ready").exists(),
        )
        assert record["status"] == "cancelled"
        assert (tmp_path / "flushed").read_text() == "saved"
    finally:
        if root:
            try:
                os.killpg(root[0], signal.SIGKILL)
            except ProcessLookupError:
                pass


def _write_fake_owner_record(module, run_dir, row, token):
    run_dir.mkdir(exist_ok=True)
    (run_dir / "dispatch-owner.json").write_text(json.dumps({
        "schema_version": 1, "kind": "dispatch", "run_dir": str(run_dir),
        "run_token": token, "owner_pid": row.pid, "owner_pgid": row.pgid,
        "owner_started_at": module._recorded_start_time(row),
    }))


@pytest.mark.parametrize("terminal_event", [False, True])
def test_forged_fabric_token_does_not_spare_command(tmp_path, monkeypatch, terminal_event):
    pid_path = tmp_path / "forged.pid"
    ready = tmp_path / "child-tracked"
    on_start = _child_census_barrier(monkeypatch, pid_path, ready)
    code = f"""import json,os,pathlib,subprocess,time
environment = dict(os.environ)
environment.update(PROVENANT_RUN_TOKEN='x', PROVENANT_RUN_DIR={str(tmp_path / 'not-a-run')!r})
child = subprocess.Popen(['sleep', '60'], start_new_session=True, env=environment,
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path({str(pid_path)!r}).write_text(str(child.pid))
while not pathlib.Path({str(ready)!r}).exists():
    time.sleep(0.01)
if {terminal_event!r}:
    print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'DONE'}}}}), flush=True)
    print(json.dumps({{'type':'turn.completed'}}), flush=True)
"""
    try:
        record = supervisor().execute(fixture_plan(tmp_path, code), tmp_path / "result.md", on_start=on_start)
        pid = int(pid_path.read_text())
        assert any(item["pid"] == pid for item in record["reaped"]), (
            record["reaped"], record.get("spared"), record["warnings"], _live_process(pid)
        )
        assert record.get("spared", 0) == 0
        assert not _live_process(pid)
    finally:
        if pid_path.exists():
            try:
                os.killpg(int(pid_path.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass


def test_tracked_pre_exec_child_is_rechecked_before_signal(tmp_path, monkeypatch):
    module = supervisor()
    root = module._ProcessRow(501, os.getpid(), 501, str(int(time.time())), "root")
    owner = module._ProcessRow(502, 501, 502, str(int(time.time())), "owner")
    rows = {os.getpid(): module._ProcessRow(os.getpid(), 1, os.getpgrp(),
                                             str(int(time.time())), "test"),
            501: root, 502: owner}
    run_dir = tmp_path / "nested-run"
    environment = []
    signals = []
    process = type("Process", (), {"pid": 501, "poll": lambda self: None})()
    with monkeypatch.context() as patch:
        patch.setattr(module, "_process_snapshot", lambda: rows)
        patch.setattr(module, "_linux_tree_snapshot", lambda *_args, **_kwargs: None)
        patch.setattr(module, "_process_environment", lambda pid: environment if pid == 502 else ())
        patch.setattr(module.os, "killpg", lambda pgid, signum: signals.append(("group", pgid)))
        patch.setattr(module.os, "kill", lambda pid, signum: signals.append(("pid", pid)))
        tracker = module._Descendants(process, "fixture")
        tracker.sample()
        assert owner.identity in tracker.tracked
        _write_fake_owner_record(module, run_dir, owner, "inner-token")
        environment[:] = [b"PROVENANT_RUN_TOKEN=inner-token",
                          ("PROVENANT_RUN_DIR=" + str(run_dir)).encode()]
        tracker.signal(signal.SIGTERM)
    assert owner.identity in tracker.spared
    assert ("group", owner.pgid) not in signals
    assert ("pid", owner.pid) not in signals


def test_owner_promoted_at_signal_is_not_reported_as_reaped(tmp_path, monkeypatch):
    module = supervisor()
    root = module._ProcessRow(501, os.getpid(), 501, str(int(time.time())), "root")
    owner = module._ProcessRow(502, 501, 502, str(int(time.time())), "owner")
    rows = {os.getpid(): module._ProcessRow(os.getpid(), 1, os.getpgrp(),
                                             str(int(time.time())), "test"),
            501: root, 502: owner}
    run_dir = tmp_path / "nested-run"
    environment = []
    signals = []
    process = type("Process", (), {"pid": 501, "poll": lambda self: None,
                                   "wait": lambda self, timeout: None})()
    with monkeypatch.context() as patch:
        patch.setattr(module, "_process_snapshot", lambda: rows)
        patch.setattr(module, "_linux_tree_snapshot", lambda *_args, **_kwargs: None)
        patch.setattr(module, "_process_environment", lambda pid: environment if pid == 502 else ())
        patch.setattr(module.os, "killpg", lambda pgid, signum: signals.append(("group", pgid)))
        patch.setattr(module.os, "kill", lambda pid, signum: signals.append(("pid", pid)))
        tracker = module._Descendants(process, "fixture")
        tracker.sample()
        original_sample = tracker.sample
        published = []

        def publish_after_snapshot(*args, **kwargs):
            snapshot = original_sample(*args, **kwargs)
            if not published:
                _write_fake_owner_record(module, run_dir, owner, "inner-token")
                environment[:] = [b"PROVENANT_RUN_TOKEN=inner-token",
                                  ("PROVENANT_RUN_DIR=" + str(run_dir)).encode()]
                published.append(True)
            return snapshot

        patch.setattr(tracker, "sample", publish_after_snapshot)
        reaped = tracker.stop(root_grace=0.1, descendant_grace=0.02)
    assert reaped == []
    assert owner.identity in tracker.spared_at_stop
    assert ("group", owner.pgid) not in signals
    assert ("pid", owner.pid) not in signals


@pytest.mark.parametrize("failure", ["env_error", "partial_record", "record_removed"])
def test_verified_owner_stays_spared_through_transient_validation_loss(tmp_path, monkeypatch, failure):
    module = supervisor()
    root_pid = os.getpid() + 100000
    owner_pid = root_pid + 1
    root = module._ProcessRow(root_pid, os.getpid(), root_pid, str(int(time.time())), "provider")
    owner = module._ProcessRow(owner_pid, root_pid, owner_pid, str(int(time.time())), "owner")
    rows = {os.getpid(): module._ProcessRow(os.getpid(), 1, os.getpgrp(),
                                             str(int(time.time())), "test"),
            root_pid: root, owner_pid: owner}
    run_dir = tmp_path / "nested-run"
    _write_fake_owner_record(module, run_dir, owner, "inner-token")
    lost = []
    environment_reads = []
    signals = []
    process = type("Process", (), {"pid": root_pid, "poll": lambda self: None,
                                   "wait": lambda self, timeout: None})()

    def environment(pid):
        if pid != owner_pid:
            return ()
        environment_reads.append(pid)
        if lost and failure == "env_error":
            raise RuntimeError("environment unavailable")
        return [b"PROVENANT_RUN_TOKEN=inner-token",
                ("PROVENANT_RUN_DIR=" + str(run_dir)).encode()]

    with monkeypatch.context() as patch:
        patch.setattr(module, "_process_snapshot", lambda: rows)
        patch.setattr(module, "_linux_tree_snapshot", lambda *_args, **_kwargs: None)
        patch.setattr(module, "_process_environment", environment)
        patch.setattr(module.os, "killpg", lambda pgid, _signal: signals.append(("group", pgid)))
        patch.setattr(module.os, "kill", lambda pid, _signal: signals.append(("pid", pid)))
        tracker = module._Descendants(process, "fixture")
        tracker.sample()
        assert owner.identity in tracker.spared
        assert tracker.verified_owner_tokens == {owner.identity: "inner-token"}
        assert environment_reads == [owner_pid]
        original_sample = tracker.sample

        def lose_after_first_stop_sample(*args, **kwargs):
            snapshot = original_sample(*args, **kwargs)
            if not lost:
                lost.append(True)
                if failure == "partial_record":
                    (run_dir / "dispatch-owner.json").write_text("{}")
                elif failure == "record_removed":
                    (run_dir / "dispatch-owner.json").unlink()
            return snapshot

        patch.setattr(tracker, "sample", lose_after_first_stop_sample)
        reaped = tracker.stop(root_grace=0.1, descendant_grace=0.02)
    assert reaped == []
    assert owner.identity in tracker.spared_at_stop
    assert ("group", owner_pid) not in signals
    assert ("pid", owner_pid) not in signals


@pytest.mark.parametrize("probe", ["unreadable", "recycled"])
def test_partial_census_keeps_a_live_verified_owner_group_spared(tmp_path, monkeypatch, probe):
    # A census that loses one row (a failed per-pid probe) must not turn the
    # owner's children into targets and killpg the owner's group with them.
    module = supervisor()
    root_pid = os.getpid() + 100000
    owner_pid, child_pid = root_pid + 1, root_pid + 2
    root = module._ProcessRow(root_pid, os.getpid(), root_pid, str(int(time.time())), "provider")
    owner = module._ProcessRow(owner_pid, root_pid, owner_pid, str(int(time.time())), "owner")
    child = module._ProcessRow(child_pid, owner_pid, owner_pid, str(int(time.time())), "nested provider")
    rows = {os.getpid(): module._ProcessRow(os.getpid(), 1, os.getpgrp(), str(int(time.time())), "test"),
            root_pid: root, owner_pid: owner, child_pid: child}
    run_dir = tmp_path / "nested-run"
    _write_fake_owner_record(module, run_dir, owner, "inner-token")
    signals = []
    # A recycled pid shows a different start time when probed on its own.
    probed = {} if probe == "unreadable" else {owner_pid: module._ProcessRow(
        owner_pid, 1, owner_pid, str(int(time.time()) + 50), "recycled")}
    process = type("Process", (), {"pid": root_pid, "poll": lambda self: None,
                                   "wait": lambda self, timeout: None})()
    with monkeypatch.context() as patch:
        patch.setattr(module, "_process_snapshot", lambda: rows)
        patch.setattr(module, "_linux_tree_snapshot", lambda *_args, **_kwargs: None)
        patch.setattr(module, "_process_environment", lambda pid: [
            b"PROVENANT_RUN_TOKEN=inner-token",
            ("PROVENANT_RUN_DIR=" + str(run_dir)).encode(),
        ] if pid == owner_pid else ())
        patch.setattr(module, "_pid_exists", lambda pid: pid in {owner_pid, child_pid, root_pid})
        patch.setattr(module, "_probe_process_row", lambda pid: probed.get(pid))
        patch.setattr(module.os, "killpg", lambda pgid, _signal: signals.append(("group", pgid)))
        patch.setattr(module.os, "kill", lambda pid, _signal: signals.append(("pid", pid)))
        tracker = module._Descendants(process, "fixture")
        tracker.sample()
        assert {owner.identity, child.identity} <= set(tracker.spared)
        del rows[owner_pid]
        tracker.stop(root_grace=0.05, descendant_grace=0.01)
    assert ("group", root_pid) in signals
    if probe == "unreadable":
        assert ("group", owner_pid) not in signals
        assert ("pid", child_pid) not in signals
    else:
        assert ("pid", child_pid) in signals


def test_unavailable_census_preserves_verified_owner_and_root_kill(tmp_path, monkeypatch):
    module = supervisor()
    root_pid = os.getpid() + 100000
    owner = module._ProcessRow(root_pid + 1, root_pid, root_pid + 1,
                               str(int(time.time())), "nested owner")
    root = module._ProcessRow(root_pid, os.getpid(), root_pid,
                              str(int(time.time())), "provider")
    rows = {os.getpid(): module._ProcessRow(os.getpid(), 1, os.getpgrp(),
                                             str(int(time.time())), "test"),
            root_pid: root, owner.pid: owner}
    run_dir = tmp_path / "nested-run"
    _write_fake_owner_record(module, run_dir, owner, "inner-token")
    signals = []
    available = [True]
    process = type("Process", (), {"pid": root_pid,
                                   "poll": lambda self: None if available[0] else 0,
                                   "wait": lambda self, timeout: None})()

    def census():
        if available[0]:
            return rows
        raise RuntimeError("census failed")

    with monkeypatch.context() as patch:
        patch.setattr(module, "_linux_tree_snapshot", lambda *_args, **_kwargs: None)
        patch.setattr(module, "_process_snapshot_unchecked", census)
        patch.setattr(module, "_process_environment", lambda pid: [
            b"PROVENANT_RUN_TOKEN=inner-token",
            ("PROVENANT_RUN_DIR=" + str(run_dir)).encode(),
        ] if pid == owner.pid else ())
        patch.setattr(module.os, "killpg", lambda pgid, _signal: signals.append(pgid))
        patch.setattr(module.os, "kill", lambda *_args: None)
        tracker = module._Descendants(process, "fixture")
        tracker.sample()
        assert owner.identity in tracker.spared
        available[0] = False
        assert module._process_snapshot() is None
        tracker.stop(root_grace=0.02, descendant_grace=0.01)
    assert tracker.snapshot_unavailable
    assert owner.identity in tracker.spared_at_stop
    assert owner.pgid not in signals
    assert root_pid in signals


def test_verified_owner_identity_does_not_spare_reused_pid(tmp_path, monkeypatch):
    module = supervisor()
    root = module._ProcessRow(501, os.getpid(), 501, str(int(time.time())), "provider")
    owner = module._ProcessRow(502, 501, 502, str(int(time.time())), "owner")
    run_dir = tmp_path / "nested-run"
    _write_fake_owner_record(module, run_dir, owner, "inner-token")
    rows = {os.getpid(): module._ProcessRow(os.getpid(), 1, os.getpgrp(),
                                             str(int(time.time())), "test"),
            501: root, 502: owner}
    process = type("Process", (), {"pid": 501, "poll": lambda self: None})()
    with monkeypatch.context() as patch:
        patch.setattr(module, "_process_snapshot", lambda: rows)
        patch.setattr(module, "_linux_tree_snapshot", lambda *_args, **_kwargs: None)
        patch.setattr(module, "_process_environment", lambda pid: [
            b"PROVENANT_RUN_TOKEN=inner-token",
            ("PROVENANT_RUN_DIR=" + str(run_dir)).encode(),
        ] if pid == 502 else ())
        tracker = module._Descendants(process, "fixture")
        tracker.sample()
        assert owner.identity in tracker.spared
        start_delta = os.sysconf("SC_CLK_TCK") * 5 if sys.platform.startswith("linux") else 5
        replacement = module._ProcessRow(502, 501, 502,
                                         str(int(owner.started) + start_delta), "replacement")
        rows[502] = replacement
        tracker.sample()
    assert replacement.identity in tracker.tracked
    assert replacement.identity not in tracker.spared


def test_provider_exit_with_open_pipe_stops_once_and_keeps_reaped(tmp_path, monkeypatch):
    module = supervisor()
    child_pid = tmp_path / "pipe-holder.pid"
    code = f"""import pathlib,subprocess,sys
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(.7)'],
    start_new_session=True, stdin=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path({str(child_pid)!r}).write_text(str(child.pid))
print('DONE', flush=True)
"""
    calls = []
    first_reaped = [{"pid": 424242, "command": "fixture-child"}]

    def fake_stop(self, *args, **kwargs):
        calls.append(kwargs)
        return first_reaped if len(calls) == 1 else []

    monkeypatch.setattr(module._Descendants, "stop", fake_stop)
    try:
        record = module.execute(fixture_plan(tmp_path, code), tmp_path / "result.md")
        assert len(calls) == 1
        assert record["reaped"] == first_reaped
    finally:
        if child_pid.exists():
            pid = int(child_pid.read_text())
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            if sys.platform.startswith("linux"):
                try:
                    os.waitpid(pid, os.WNOHANG)
                except ChildProcessError:
                    pass


def test_token_without_owner_record_is_not_nested_owner(tmp_path, monkeypatch):
    module = supervisor()
    row = module._ProcessRow(502, 501, 502, str(int(time.time())), "sleep")
    monkeypatch.setattr(module, "_process_environment", lambda _pid: [
        b"PROVENANT_RUN_TOKEN=x",
        ("PROVENANT_RUN_DIR=" + str(tmp_path / "absent-run")).encode(),
    ])
    assert not module._nested_fabric_owner_token(row)


def test_cleanup_does_not_adopt_marker_host_or_signal_a_descendants_shared_group(monkeypatch):
    module = supervisor()
    root = module._ProcessRow(501, os.getpid(), 501, str(int(time.time())), "provider")
    child = module._ProcessRow(502, 501, 900, str(int(time.time())), "node")
    sibling = module._ProcessRow(503, 999, 900, str(int(time.time())), "sibling provider")
    host = module._ProcessRow(504, 1, 901, str(int(time.time())), "codex-code-mode-host")
    rows = {row.pid: row for row in (root, child, sibling, host)}
    rows[os.getpid()] = module._ProcessRow(os.getpid(), 1, os.getpgrp(), "0", "test")
    process = type("Process", (), {"pid": 501, "poll": lambda self: None})()
    signals = []
    monkeypatch.setattr(module, "_linux_tree_snapshot", lambda *_a, **_kw: None)
    monkeypatch.setattr(module, "_process_snapshot", lambda: rows)
    monkeypatch.setattr(module, "_has_attempt_marker", lambda *_a: True)
    monkeypatch.setattr(module.os, "killpg", lambda pid, sig: signals.append(("group", pid, sig)))
    monkeypatch.setattr(module.os, "kill", lambda pid, sig: signals.append(("pid", pid, sig)))
    tracker = module._Descendants(process, "fixture")
    tracker.spawned_at = tracker.spawned_ticks = 0
    tracker.sample(include_reparented=True)
    tracker.signal(signal.SIGTERM)
    assert child.identity in tracker.tracked
    assert host.identity not in tracker.tracked
    assert signals == [("group", 501, signal.SIGTERM), ("pid", 502, signal.SIGTERM)]


@pytest.mark.parametrize("host_group", [501, 504])
@pytest.mark.parametrize("command", ["codex-code-mode-host", "codex-code-mode"])
def test_observed_code_mode_host_and_its_sibling_lanes_are_never_signalled(monkeypatch, host_group, command):
    module = supervisor()
    root = module._ProcessRow(501, os.getpid(), 501, "1", "provider")
    host = module._ProcessRow(504, 501, host_group, "2", command)
    sibling = module._ProcessRow(505, 504, 505, "3", "sibling provider")
    rows = {row.pid: row for row in (root, host, sibling)}
    rows[os.getpid()] = module._ProcessRow(os.getpid(), 1, os.getpgrp(), "0", "test")
    process = type("Process", (), {"pid": 501, "poll": lambda self: None})()
    signals = []
    monkeypatch.setattr(module, "_linux_tree_snapshot", lambda *_a, **_kw: None)
    monkeypatch.setattr(module, "_process_snapshot", lambda: rows)
    monkeypatch.setattr(module.os, "killpg", lambda pid, sig: signals.append(("group", pid)))
    monkeypatch.setattr(module.os, "kill", lambda pid, sig: signals.append(("pid", pid)))
    tracker = module._Descendants(process, "fixture")
    tracker.sample()
    tracker.signal(signal.SIGTERM)
    assert {host.identity, sibling.identity} <= set(tracker.spared)
    assert ("pid", host.pid) not in signals and ("pid", sibling.pid) not in signals
    assert ("group", host_group) not in signals
    assert ("pid", root.pid) in signals


def test_shared_host_group_change_then_missing_census_never_allows_group_signal(monkeypatch):
    module = supervisor()
    root = module._ProcessRow(501, os.getpid(), 501, "1", "provider")
    host = module._ProcessRow(504, 501, 504, "2", "codex-code-mode-host")
    rows = {501: root, 504: host, os.getpid(): module._ProcessRow(os.getpid(), 1, os.getpgrp(), "0", "test")}
    process = type("Process", (), {"pid": 501, "poll": lambda self: None})()
    signals = []
    monkeypatch.setattr(module, "_linux_tree_snapshot", lambda *_a, **_kw: None)
    monkeypatch.setattr(module, "_process_snapshot", lambda: rows)
    monkeypatch.setattr(module, "_probe_process_row", lambda _pid: None)
    monkeypatch.setattr(module, "_pid_exists", lambda _pid: True)
    monkeypatch.setattr(module.os, "killpg", lambda pid, sig: signals.append(("group", pid)))
    monkeypatch.setattr(module.os, "kill", lambda pid, sig: signals.append(("pid", pid)))
    tracker = module._Descendants(process, "fixture")
    tracker.sample()
    rows[504] = module._ProcessRow(504, 501, 501, "2", "codex-code-mode-host")
    tracker.sample()
    del rows[504]
    tracker.signal(signal.SIGTERM)
    assert signals and set(signals) == {("pid", 501)}


@pytest.mark.parametrize("terminal_event", [False, True])
@pytest.mark.parametrize("controller", [None, "orphan_reaper", "cancel_requested"],
                         ids=["unknown-sender", "orphan-reaper", "markerless-cancel"])
def test_external_provider_sigterm_without_a_result_fails_unless_cancelled(tmp_path, terminal_event, controller):
    module = supervisor()
    events = [{"type": "ready"}] + ([{"type": "turn.completed"}] if terminal_event else [])
    code = "import os,time\nos.write(1, " + repr(("\n".join(map(json.dumps, events)) + "\n").encode()) + ")\ntime.sleep(60)\n"
    provider = []
    signalled = []

    def interrupt(_timestamp):
        if not signalled:
            signalled.append(True)
            if controller:
                (tmp_path / "termination-request.json").write_text(json.dumps({
                    "run_token": "fixture-control", "signal": "SIGTERM", "source": controller,
                    "sender_pid": os.getpid(), "provider_pid": provider[0].pid,
                    "requested_at": module.now(),
                }))
            os.kill(provider[0].pid, signal.SIGTERM)

    record = module.execute(fixture_plan(tmp_path, code), tmp_path / "result.md",
                            on_start=provider.append, on_progress=interrupt,
                            env={**os.environ, "PROVENANT_RUN_DIR": str(tmp_path),
                                 "PROVENANT_RUN_TOKEN": "fixture-control"})
    assert record["status"] == ("cancelled" if controller == "cancel_requested" else "failed"), record
    assert record["exit"] == -signal.SIGTERM
    assert record["evidence"]["signal"] == signal.SIGTERM
    assert record["evidence"]["signal_name"] == "SIGTERM"
    assert record["evidence"]["termination_source"] == (controller or "external_signal")
    assert record["evidence"]["signal_requester_pid"] == (os.getpid() if controller else None)
    assert record["evidence"]["signal_sender_pid"] is None
    assert record["evidence"]["signature"] == ("cancel_requested" if controller == "cancel_requested" else "provider_signal")
    assert "error" not in record


@pytest.mark.parametrize("invalid", ["run_token", "provider_pid", "null_pid", "signal", "requested_at", "symlink"])
def test_signal_receipt_does_not_attribute_mismatching_or_linked_control_telemetry(tmp_path, invalid):
    module = supervisor()
    provider = []

    def interrupt(_timestamp):
        if provider[0].poll() is not None or (tmp_path / "termination-request.json").is_symlink():
            return  # on_progress fires per output chunk or CPU change; symlink_to would raise on a repeat.
        request = {"run_token": "current", "signal": "SIGTERM", "source": "orphan_reaper",
                   "sender_pid": os.getpid(), "provider_pid": provider[0].pid, "requested_at": module.now()}
        if invalid != "symlink":
            request["provider_pid" if invalid == "null_pid" else invalid] = {
                "run_token": "earlier", "provider_pid": -1, "null_pid": None, "signal": "SIGKILL",
                "requested_at": "2000-01-01T00:00:00Z"}[invalid]
        path = tmp_path / "termination-request.json"
        if invalid == "symlink":
            target = tmp_path / "outside-request.json"
            target.write_text(json.dumps(request))
            path.symlink_to(target)
        else:
            path.write_text(json.dumps(request))
        os.kill(provider[0].pid, signal.SIGTERM)

    record = module.execute(fixture_plan(tmp_path, "import time\nprint('ready', flush=True)\ntime.sleep(60)"),
                            tmp_path / "result.md", on_start=provider.append, on_progress=interrupt,
                            env={**os.environ, "PROVENANT_RUN_DIR": str(tmp_path), "PROVENANT_RUN_TOKEN": "current"})
    assert record["status"] == "failed"
    assert record["evidence"]["termination_source"] == "external_signal"
    assert record["evidence"]["signal_requester_pid"] is None


def test_supervisor_signal_is_interrupted_without_a_cancel_request(tmp_path):
    record = supervisor().execute(
        fixture_plan(tmp_path, "import time\nprint('ready', flush=True)\ntime.sleep(60)"), tmp_path / "result.md",
        on_progress=lambda _timestamp: os.kill(os.getpid(), signal.SIGHUP),
    )
    assert record["status"] == "interrupted"
    assert record["evidence"]["supervisor_signal"] == signal.SIGHUP
    assert record["evidence"]["termination_source"] == "supervisor_signal"


def test_markerless_cancel_keeps_cancelled_when_provider_flushes_and_exits_zero(tmp_path):
    module = supervisor()
    provider = []
    ready = tmp_path / "provider-ready"

    def interrupt(_timestamp):
        if not ready.exists():
            return
        (tmp_path / "termination-request.json").write_text(json.dumps({
            "run_token": "graceful", "signal": "SIGTERM", "source": "cancel_requested",
            "sender_pid": os.getpid(), "provider_pid": provider[0].pid, "requested_at": module.now(),
        }))
        os.kill(os.getpid(), signal.SIGTERM)  # This execute() owns the installed signal handler.
        os.kill(provider[0].pid, signal.SIGTERM)

    code = ("import signal,time\nsignal.signal(signal.SIGTERM, lambda *_: exit(0))\n"
            f"open({str(ready)!r}, 'w').close()\nprint('ready', flush=True)\ntime.sleep(60)")
    record = module.execute(fixture_plan(tmp_path, code), tmp_path / "result.md",
                            on_start=provider.append, on_progress=interrupt,
                            env={**os.environ, "PROVENANT_RUN_DIR": str(tmp_path), "PROVENANT_RUN_TOKEN": "graceful"})
    assert record["status"] == "cancelled", record
    assert record["exit"] == 0
    assert record["evidence"]["signal"] is None
    assert record["evidence"]["supervisor_signal"] == signal.SIGTERM
    assert record["evidence"]["termination_source"] == "cancel_requested"


def test_owner_validation_error_cannot_prevent_root_group_kill(monkeypatch):
    module = supervisor()
    process = type("Process", (), {"pid": 501, "poll": lambda self: None})()
    root = module._ProcessRow(501, os.getpid(), 501, str(int(time.time())), "root")
    tracker = module._Descendants(process, "fixture")
    tracker.root = root.identity
    tracker.tracked[root.identity] = root
    groups = []
    with monkeypatch.context() as patch:
        patch.setattr(module, "_process_snapshot", lambda: {501: root})
        patch.setattr(module, "_process_environment",
                      lambda _pid: (_ for _ in ()).throw(RuntimeError("env failed")))
        patch.setattr(module.os, "killpg", lambda pgid, _signal: groups.append(pgid))
        patch.setattr(module.os, "kill", lambda *_args: None)
        tracker.signal(signal.SIGTERM)
    assert 501 in groups


def test_forged_same_group_owner_cannot_suppress_provider_kill(tmp_path, monkeypatch):
    module = supervisor()
    root_pid = os.getpid() + 100000
    child_pid = root_pid + 1
    root = module._ProcessRow(root_pid, os.getpid(), root_pid, str(int(time.time())), "provider")
    child = module._ProcessRow(child_pid, root_pid, root_pid, str(int(time.time())), "forged")
    rows = {os.getpid(): module._ProcessRow(os.getpid(), 1, os.getpgrp(),
                                             str(int(time.time())), "test"),
            root_pid: root, child_pid: child}
    run_dir = tmp_path / "forged-run"
    _write_fake_owner_record(module, run_dir, child, "forged-token")
    signals = []
    process = type("Process", (), {"pid": root_pid, "poll": lambda self: None})()
    with monkeypatch.context() as patch:
        patch.setattr(module, "_process_snapshot", lambda: rows)
        patch.setattr(module, "_linux_tree_snapshot", lambda *_args, **_kwargs: None)
        patch.setattr(module, "_process_environment", lambda pid: [
            b"PROVENANT_RUN_TOKEN=forged-token",
            ("PROVENANT_RUN_DIR=" + str(run_dir)).encode(),
        ] if pid == child_pid else ())
        patch.setattr(module.os, "killpg", lambda pgid, _signal: signals.append(pgid))
        patch.setattr(module.os, "kill", lambda *_args: None)
        tracker = module._Descendants(process, "fixture")
        tracker.sample()
        tracker.signal(signal.SIGTERM)
    assert root_pid in signals
    assert child.identity not in tracker.spared


def test_owner_without_observed_parent_is_not_spared(tmp_path, monkeypatch):
    module = supervisor()
    row = module._ProcessRow(502, 999, 502, str(int(time.time())), "claimed-owner")
    run_dir = tmp_path / "forged-run"
    _write_fake_owner_record(module, run_dir, row, "forged-token")
    monkeypatch.setattr(module, "_process_environment", lambda _pid: [
        b"PROVENANT_RUN_TOKEN=forged-token",
        ("PROVENANT_RUN_DIR=" + str(run_dir)).encode(),
    ])
    process = type("Process", (), {"pid": 501, "poll": lambda self: None})()
    tracker = module._Descendants(process, "fixture")
    tracker.tracked[row.identity] = row
    tracker._refresh_spared({row.pid: row})
    assert row.identity not in tracker.spared


def test_reparented_marker_owner_without_ancestry_is_not_adopted(tmp_path, monkeypatch):
    module = supervisor()
    root_pid = os.getpid() + 100000
    owner = module._ProcessRow(root_pid + 1, 1, root_pid + 1,
                               str(int(time.time())), "nested owner")
    run_dir = tmp_path / "nested-run"
    _write_fake_owner_record(module, run_dir, owner, "inner-token")
    rows = {os.getpid(): module._ProcessRow(os.getpid(), 1, os.getpgrp(),
                                             str(int(time.time())), "test"), owner.pid: owner}
    process = type("Process", (), {"pid": root_pid, "poll": lambda self: 0})()
    signals = []
    with monkeypatch.context() as patch:
        patch.setattr(module, "_process_snapshot", lambda: rows)
        patch.setattr(module, "_has_attempt_marker", lambda pid, _marker: pid == owner.pid)
        patch.setattr(module, "_process_environment", lambda pid: [
            b"PROVENANT_RUN_TOKEN=inner-token",
            ("PROVENANT_RUN_DIR=" + str(run_dir)).encode(),
        ] if pid == owner.pid else ())
        patch.setattr(module.os, "killpg", lambda pgid, _signal: signals.append(pgid))
        patch.setattr(module.os, "kill", lambda *_args: None)
        tracker = module._Descendants(process, "fixture")
        tracker.spawned_at = 0
        tracker.spawned_ticks = 0
        tracker.sample(include_reparented=True)
        assert owner.identity not in tracker.spared
        assert owner.identity not in tracker.tracked
        tracker.signal(signal.SIGTERM)
    assert owner.pgid not in signals
    assert root_pid in signals


@pytest.mark.parametrize("field,value", [
    ("owner_pid", 999999), ("owner_started_at", "old process"),
    ("owner_started_at", None), ("run_token", "wrong token"),
])
def test_nested_owner_record_must_match_live_identity(tmp_path, monkeypatch, field, value):
    module = supervisor()
    row = module._ProcessRow(502, 501, 502, str(int(time.time())), "owner")
    run_dir = tmp_path / "nested-run"
    _write_fake_owner_record(module, run_dir, row, "inner-token")
    path = run_dir / "dispatch-owner.json"
    record = json.loads(path.read_text())
    record[field] = value
    path.write_text(json.dumps(record))
    monkeypatch.setattr(module, "_process_environment", lambda _pid: [
        b"PROVENANT_RUN_TOKEN=inner-token",
        ("PROVENANT_RUN_DIR=" + str(run_dir)).encode(),
    ])
    assert not module._nested_fabric_owner_token(row)


def test_nested_owner_accepts_legacy_inherited_locale_start(tmp_path, monkeypatch):
    module = supervisor()
    row = module._ProcessRow(502, 501, 502, str(int(time.time())), "owner")
    run_dir = tmp_path / "nested-run"
    _write_fake_owner_record(module, run_dir, row, "inner-token")
    path = run_dir / "dispatch-owner.json"
    record = json.loads(path.read_text())
    weekday, month, day, clock, year = record["owner_started_at"].split()
    legacy = f"{weekday} {day} {month} {clock} {year}"
    record["owner_started_at"] = legacy
    path.write_text(json.dumps(record))
    monkeypatch.setenv("LC_ALL", "en_AU.UTF-8")
    monkeypatch.setenv("LANG", "en_AU.UTF-8")
    monkeypatch.setattr(module, "_process_environment", lambda _pid: [
        b"PROVENANT_RUN_TOKEN=inner-token",
        ("PROVENANT_RUN_DIR=" + str(run_dir)).encode(),
    ])
    calls = []

    def locale_ps(argv, **kwargs):
        calls.append((argv, kwargs))
        output = record["owner_started_at"] if kwargs.get("env", {}).get("LC_ALL") != "C" else module._recorded_start_time(row)
        return subprocess.CompletedProcess(argv, 0, stdout=output)

    # The fake pid may be a live process on the host; force the ps path under test.
    monkeypatch.setattr(module.process_info, "process", lambda _pid: None)
    monkeypatch.setattr(module.subprocess, "run", locale_ps)
    assert module._nested_fabric_owner_token(row)
    assert [call[1].get("env", {}).get("LC_ALL") for call in calls] == ["C", "en_AU.UTF-8"]


def test_nested_owner_accepts_canonical_ps_when_computed_start_differs(tmp_path, monkeypatch):
    module = supervisor()
    row = module._ProcessRow(502, 501, 502, str(int(time.time())), "owner")
    run_dir = tmp_path / "nested-run"
    _write_fake_owner_record(module, run_dir, row, "inner-token")
    canonical = json.loads((run_dir / "dispatch-owner.json").read_text())["owner_started_at"]
    monkeypatch.setattr(module, "_recorded_start_time", lambda _row: "rounded differently")
    monkeypatch.setattr(module, "_process_environment", lambda _pid: [
        b"PROVENANT_RUN_TOKEN=inner-token",
        ("PROVENANT_RUN_DIR=" + str(run_dir)).encode(),
    ])
    calls = []

    def canonical_ps(argv, **kwargs):
        calls.append(kwargs.get("env", {}).get("LC_ALL"))
        return subprocess.CompletedProcess(argv, 0, stdout=canonical if calls[-1] == "C" else "other locale")

    # The fake pid may be a live process on the host; force the ps path under test.
    monkeypatch.setattr(module.process_info, "process", lambda _pid: None)
    monkeypatch.setattr(module.subprocess, "run", canonical_ps)
    assert module._nested_fabric_owner_token(row)
    assert calls == ["C"]


@pytest.mark.parametrize("stop", ["normal", "cancelled"])
def test_nested_fabric_owner_and_its_child_are_spared(tmp_path, stop):
    inner = """import pathlib,subprocess,time
pathlib.Path('inner-ready').touch()
child = subprocess.Popen(['sleep', '60'], start_new_session=True,
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path('inner-child.pid').write_text(str(child.pid))
time.sleep(2.5)
child.terminate()
child.wait(timeout=3)
pathlib.Path('inner-terminal').write_text('ok')
"""
    run_dir = tmp_path / "nested-run"
    outer = f"""import json,os,pathlib,subprocess,sys,time
environment = dict(os.environ)
environment['PROVENANT_RUN_TOKEN'] = 'nested-owner-fixture'
environment['PROVENANT_RUN_DIR'] = {str(run_dir)!r}
owner = subprocess.Popen([sys.executable, '-c', {inner!r}],
    start_new_session=True, env=environment, stdin=subprocess.DEVNULL,
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path('inner-owner.pid').write_text(str(owner.pid))
print(json.dumps({{'type':'owner.ready'}}), flush=True)
if {stop!r} == 'normal':
    print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'DONE'}}}}), flush=True)
    print(json.dumps({{'type':'turn.completed'}}), flush=True)
    time.sleep(.5)
else:
    time.sleep(60)
"""
    plan = fixture_plan(tmp_path, outer, timeout_seconds=8)
    recorded = []

    def record_owner(_timestamp):
        if recorded or not (tmp_path / "inner-owner.pid").exists():
            return
        pid = int((tmp_path / "inner-owner.pid").read_text())
        row = supervisor()._process_snapshot().get(pid)
        assert row is not None
        _write_fake_owner_record(supervisor(), run_dir, row, "nested-owner-fixture")
        recorded.append(True)

    try:
        record = supervisor().execute(
            plan, tmp_path / "result.md",
            on_progress=record_owner,
            cancelled=lambda: stop == "cancelled" and (tmp_path / "inner-owner.pid").exists(),
        )
        assert recorded
        assert record["status"] == ("ok" if stop == "normal" else "cancelled")
        assert record["spared"] >= 1
        assert record["reaped"] == []
        deadline = time.monotonic() + 4
        while not (tmp_path / "inner-terminal").exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        assert (tmp_path / "inner-terminal").read_text() == "ok"
    finally:
        for name in ("inner-owner.pid", "inner-child.pid"):
            path = tmp_path / name
            if path.exists():
                try:
                    os.killpg(int(path.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass


def test_terminal_grace_with_live_claude_skips_settle_and_eof_child(tmp_path, monkeypatch):
    module = supervisor()
    child_pid = tmp_path / "mcp-child.pid"
    code = f"""import json,pathlib,signal,subprocess,sys,time
child = subprocess.Popen([sys.executable, '-c', 'import sys; sys.stdin.read()'],
    stdin=subprocess.PIPE,
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path({str(child_pid)!r}).write_text(str(child.pid))
def stop(*_args):
    child.stdin.close()
    raise SystemExit(0)
signal.signal(signal.SIGTERM, stop)
print(json.dumps({{'type':'result','is_error':False,'result':'DONE'}}), flush=True)
time.sleep(60)
"""
    modes = []
    saw_child = []
    original_stop = module._Descendants.stop

    def observed_stop(self, *args, **kwargs):
        modes.append(kwargs.get("normal"))
        rows = self.sample(include_reparented=True)
        saw_child.append(any(row.pid == int(child_pid.read_text())
                             for row in self.live(rows).values()))
        return original_stop(self, *args, **kwargs)

    monkeypatch.setattr(module._Descendants, "stop", observed_stop)
    try:
        record = module.execute(fixture_plan(tmp_path, code, "claude"), tmp_path / "result.md")
        assert record["status"] == "ok"
        assert saw_child == [True]
        assert modes == [False]
        assert record["reaped"] == []
    finally:
        if child_pid.exists():
            try:
                os.killpg(int(child_pid.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass


@pytest.mark.parametrize(
    "adapter", ["claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot"]
)
def test_all_adapters_wall_deadline_terminates_silent_provider(tmp_path, adapter):
    plan = fixture_plan(tmp_path, 'import time; time.sleep(60)', adapter,
                        timeout_seconds=0.2, startup_seconds=30)
    record = supervisor().execute(plan, tmp_path / 'result.md',
                                  env={**os.environ, 'CF_DISPATCH_ENABLE_COPILOT': '1'})
    assert record['status'] == 'timed_out'
    assert record['exit'] is not None
    assert record['evidence']['signature'] == 'wall_clock'


@pytest.mark.parametrize('error_kind,expected', [('os', 'failed'), ('owned', 'rejected')])
def test_capture_registration_failure_returns_typed_receipt(tmp_path, monkeypatch, error_kind, expected):
    module = supervisor()
    plan = fixture_plan(tmp_path, 'print("DONE")')
    def unavailable(*args):
        raise (OSError('capture registration failed') if error_kind == 'os'
               else module.stall_watchdog.OwnedFileError('unsafe capture registration'))
    monkeypatch.setattr(module.stall_watchdog, 'register_capture', unavailable)
    record = module.execute(plan, tmp_path / 'result.md')
    assert record['adapter'] == 'codex'
    assert record['execution_intent'] == 'ordinary'
    assert record['status'] == expected
    assert record['error_class'] != 'none'


@pytest.mark.parametrize(
    "adapter", ["claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot"]
)
def test_all_adapters_observe_silence_without_killing_live_provider(tmp_path, adapter):
    code = "import time; print('started', flush=True); time.sleep(1.4); print('finished', flush=True)"
    plan = fixture_plan(tmp_path, code, adapter, timeout_seconds=3)
    record = supervisor().execute(
        plan, tmp_path / "result.md",
        env={**os.environ, "CF_DISPATCH_ENABLE_COPILOT": "1",
             "PROVENANT_KNOB_LANES_SILENCE_SECONDS": "1"},
    )
    observation = json.loads((tmp_path / "watchdog.json").read_text())
    assert observation["process_alive"] is True
    assert observation["last_event_at"] is not None
    assert observation["last_worktree_write_at"] is not None
    assert observation["silent_seconds"] >= 1
    assert record["status"] == "ok"
    assert record["exit"] == 0


def test_retry_watchdog_and_checkpoint_read_current_capture(tmp_path):
    module = supervisor()
    events = tmp_path / "events.jsonl"
    events.write_text('{"type":"tool_call","name":"old"}\n')
    (tmp_path / "stderr.log").write_text("old diagnostic")
    os.utime(events, (time.time() - 600, time.time() - 600))
    os.utime(tmp_path / "stderr.log", (time.time() - 600, time.time() - 600))
    plan = fixture_plan(tmp_path,
        'import time; print(\'{"type":"tool_call","name":"current"}\', flush=True); time.sleep(1.4)',
        "claude", timeout_seconds=3)
    plan["silence_seconds"] = 1
    record = module.execute(plan, tmp_path / "result.md")
    observation = json.loads((tmp_path / "watchdog.json").read_text())
    assert observation["last_event_at"] != module.stall_watchdog.timestamp(events.stat().st_mtime)
    (tmp_path / "watchdog.json").unlink()
    module.stall_watchdog.observe_if_silent(tmp_path, tmp_path, record["pgid"], True,
                                           "2000-01-01T00:00:00Z", 300)
    assert not (tmp_path / "watchdog.json").exists()
    module.stall_watchdog.checkpoint(tmp_path, tmp_path, record["pgid"])
    checkpoint = json.loads((tmp_path / "wip-checkpoint.json").read_text())
    assert '"name": "current"' in checkpoint["last_tool_call"]


def test_silent_provider_fails_as_startup_timeout_and_is_reaped(tmp_path):
    child_pid = tmp_path / "child.pid"
    code = f"""import pathlib,subprocess,sys,time
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])
pathlib.Path({str(child_pid)!r}).write_text(str(child.pid))
print('opencode banner', file=sys.stderr, flush=True)
time.sleep(60)
"""
    plan = fixture_plan(tmp_path, code, "opencode", startup_seconds=0.6, timeout_seconds=30)
    started = time.monotonic()
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert time.monotonic() - started < 10
    assert record["status"] == "startup_timeout"
    assert record["evidence"]["signature"] == "startup_watchdog"
    assert record["retryable"] is True
    assert "re-route" in record["fix"]
    assert "no provider output within 0.6s" in (tmp_path / "result.md").read_text()
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        try:
            os.kill(int(child_pid.read_text()), 0)
        except ProcessLookupError:
            break
        time.sleep(0.05)
    else:
        pytest.fail("provider child survived startup_timeout")


@pytest.mark.parametrize("stream", ["stderr", "error_event"])
@pytest.mark.parametrize("reset", ["Resets in 2 days.", ""])
def test_opencode_quota_hang_fails_fast_and_cools_only_its_model(tmp_path, stream, reset):
    message = "Monthly usage limit reached. " + reset
    payload = (json.dumps({"type": "error", "error": {"message": message}})
               if stream == "error_event" else "ERROR " + "context=" + "x" * 220 + " error=" + message)
    code = f"""import sys,time
sys.{ 'stdout' if stream == 'error_event' else 'stderr' }.write({payload[:25]!r})
sys.{ 'stdout' if stream == 'error_event' else 'stderr' }.flush()
time.sleep(0.1)
sys.{ 'stdout' if stream == 'error_event' else 'stderr' }.write({(payload[25:] + chr(10))!r})
sys.{ 'stdout' if stream == 'error_event' else 'stderr' }.flush()
time.sleep(30)
"""
    plan = fixture_plan(tmp_path, code, "opencode", startup_seconds=0.6, timeout_seconds=3)
    plan["model"] = "opencode-go/glm-5"
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "usage_limited"
    assert record["evidence"]["signature"] == "quota_exhausted"
    assert record["evidence"]["termination_source"] == "usage_limited"
    assert record["retryable"] is True
    assert record["retry_after"] == (None if reset else 24 * 3600)
    from skills.orchestrate.scripts import fabric_records
    record["run_id"] = "fixture"
    path = tmp_path / "cooldowns.json"
    at = datetime.now(UTC)
    fabric_records.write_cooldown(record, path=path, at=at)
    cooldown = fabric_records.read_cooldowns(path, at)
    assert set(cooldown) == {"opencode/opencode-go/glm-5"}
    until = fabric_records.parse_time(cooldown["opencode/opencode-go/glm-5"]["cooling_until"])
    if reset:
        assert timedelta(days=1) < until - at < timedelta(days=3)
    else:
        assert until - at == timedelta(hours=24)


def test_opencode_launch_exposes_error_logs_for_silent_quota_hangs(tmp_path):
    from adapters import opencode
    plan = fixture_plan(tmp_path, "pass", "opencode")
    command = opencode.argv(plan)
    assert "--print-logs" in command
    assert command[command.index("--log-level") + 1] == "error"


@pytest.mark.parametrize("stream", ["stdout", "stderr"])
def test_kiro_browser_login_hang_fails_fast_with_auth_fix(tmp_path, stream):
    code = f"""import sys,time
sys.{stream}.write('Opening brow')
sys.{stream}.flush()
time.sleep(0.1)
sys.{stream}.write('ser...\\n')
sys.{stream}.flush()
time.sleep(30)
"""
    plan = fixture_plan(tmp_path, code, "kiro", startup_seconds=0.6, timeout_seconds=3)
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "auth_required"
    assert record["evidence"]["signature"] == "auth_expired"
    assert record["evidence"]["termination_source"] == "auth_required"
    assert "kiro-cli whoami" in record["fix"]
    assert "outside the sandbox" in record["fix"]


@pytest.mark.parametrize("adapter,text", [("opencode", "Monthly usage limit reached. Resets in 2 days."),
                                          ("kiro", "Opening browser...")])
def test_failure_phrases_in_assistant_prose_are_not_live_errors(tmp_path, adapter, text):
    event = json.dumps({"type": "text", "text": text})
    plan = fixture_plan(tmp_path, f"print({event!r})", adapter)
    assert supervisor().execute(plan, tmp_path / "result.md")["status"] == "ok"


def test_provider_output_before_startup_window_keeps_running(tmp_path, provider_clock):
    ready = tmp_path / "startup-elapsed"
    advanced = []

    def progress(_timestamp):
        if not advanced:
            provider_clock(1.5)
            advanced.append(True)

    def release():
        if advanced:
            ready.touch()
        return False

    code = f"""import json,pathlib,time
print(json.dumps({{'type':'thread.started','thread_id':'t1'}}), flush=True)
while not pathlib.Path({str(ready)!r}).exists():
    time.sleep(0.01)
print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'DONE'}}}}), flush=True)
print(json.dumps({{'type':'turn.completed'}}), flush=True)
"""
    plan = fixture_plan(tmp_path, code, startup_seconds=0.6, timeout_seconds=30)
    record = supervisor().execute(plan, tmp_path / "result.md", on_progress=progress, cancelled=release)
    assert record["status"] == "ok"
    assert (tmp_path / "result.md").read_text() == "DONE"


def test_startup_window_defaults_to_five_minutes_and_reads_environment(tmp_path, monkeypatch):
    monkeypatch.delenv("CF_DISPATCH_STARTUP_SECONDS", raising=False)
    assert fixture_plan(tmp_path, "pass")["startup_seconds"] == 300
    monkeypatch.setenv("CF_DISPATCH_STARTUP_SECONDS", "45")
    assert fixture_plan(tmp_path, "pass")["startup_seconds"] == 45
    monkeypatch.setenv("CF_DISPATCH_STARTUP_SECONDS", "-1")
    with pytest.raises(ValueError, match="finite positive"):
        fixture_plan(tmp_path, "pass")


def test_cursor_terminal_event_closes_open_stdin_and_reaps_descendants(tmp_path):
    code = """import json,os,stat,subprocess,sys,time
assert stat.S_ISFIFO(os.fstat(0).st_mode)
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(30)'])
print(json.dumps({'type':'system','subtype':'init','model':'grok','session_id':'cursor-1'}))
print(json.dumps({'type':'result','result':'DONE','is_error':False}))
time.sleep(30)
"""
    plan = fixture_plan(tmp_path, code, "cursor", timeout_seconds=2)
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert (tmp_path / "result.md").read_text() == "DONE"
    assert record["session_id"] == "cursor-1"
    assert record["provenance"]["observed_model"] == "grok"
    assert record["provenance"]["identity"] == "observed"


def test_claude_resume_preamble_result_does_not_end_the_run(tmp_path):
    # A resumed Claude session first settles leftover background-task
    # notifications with an empty result, then starts the real turn.
    code = """import json,time
emit=lambda e: print(json.dumps(e), flush=True)
emit({'type':'system','subtype':'task_notification','task_id':'b1','status':'stopped'})
emit({'type':'system','subtype':'init','model':'claude-opus-5-5','session_id':'s-1'})
emit({'type':'result','subtype':'success','result':'','is_error':False,'num_turns':0})
emit({'type':'system','subtype':'init','model':'claude-opus-5-5','session_id':'s-1'})
emit({'type':'assistant','message':{'content':[{'type':'text','text':'working'}]}})
emit({'type':'result','subtype':'success','result':'DONE','is_error':False,'num_turns':3})
"""
    plan = fixture_plan(tmp_path, code, "claude", timeout_seconds=10)
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert (tmp_path / "result.md").read_text() == "DONE"


def test_claude_resume_preamble_does_not_start_terminal_grace(tmp_path, provider_clock):
    # The real turn can start later than the grace period after the empty
    # preamble result, for example while a large session reloads.
    preamble = tmp_path / "preamble-result-seen"
    resume = tmp_path / "resume-session"
    advanced = []

    def progress(_timestamp):
        if preamble.exists() and not advanced:
            provider_clock(0.4)
            advanced.append(True)
            resume.touch()

    code = f"""import json,pathlib,sys,time
emit=lambda e: print(json.dumps(e), flush=True)
emit({{'type':'system','subtype':'init','model':'claude-opus-5-5','session_id':'s-1'}})
emit({{'type':'result','subtype':'success','result':'','is_error':False,'num_turns':0}})
pathlib.Path({str(preamble)!r}).touch()
print('reloading session', file=sys.stderr, flush=True)  # progress after the marker exists
while not pathlib.Path({str(resume)!r}).exists():
    time.sleep(0.01)
emit({{'type':'system','subtype':'init','model':'claude-opus-5-5','session_id':'s-1'}})
emit({{'type':'assistant','message':{{'content':[{{'type':'text','text':'working'}}]}}}})
emit({{'type':'result','subtype':'success','result':'DONE','is_error':False,'num_turns':3}})
"""
    plan = fixture_plan(tmp_path, code, "claude", timeout_seconds=10)
    plan["grace_seconds"] = 0.3
    record = supervisor().execute(plan, tmp_path / "result.md", on_progress=progress)
    assert record["status"] == "ok"
    assert (tmp_path / "result.md").read_text() == "DONE"


def test_wall_timeout_terminates_without_output(tmp_path):
    plan = fixture_plan(
        tmp_path, "import time; time.sleep(30)", timeout_seconds=0.2
    )
    assert supervisor().execute(plan, tmp_path / "result.md")["status"] == "timed_out"


def test_observed_substitution_cannot_certify_the_resolved_family(tmp_path):
    plan = fixture_plan(
        tmp_path,
        "import json; print(json.dumps({'type':'system','subtype':'init','model':'other-vendor'})); "
        "print(json.dumps({'type':'result','result':'DONE','is_error':False}))",
        "claude",
        intent="assurance",
        orchestrator_family="anthropic",
    )
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["provenance"]["family"] == "unknown"
    assert not record["cross_family"]
    assert not record["certification_eligible"]


@pytest.mark.parametrize("observed,expected_family,catalogued", [
    pytest.param("claude-sonnet-5-5", "anthropic", True, id="catalogued-family"),
    pytest.param("gpt-claude-model", "unknown", False, id="ambiguous-family"),
])
def test_observed_substitution_records_only_a_verified_answering_family(
    tmp_path, observed, expected_family, catalogued,
):
    code = f"""import json
events = [
    {{"type":"system","subtype":"init","model":"claude-haiku-4-5-20251001","session_id":"s-1"}},
    {{"type":"assistant","message":{{"model":{observed!r},"content":[{{"type":"text","text":"DONE"}}]}}}},
    {{"type":"result","result":"DONE","is_error":False,"session_id":"s-1"}},
]
for event in events:
    print(json.dumps(event), flush=True)
"""
    plan = fixture_plan(
        tmp_path,
        code,
        "claude",
        intent="assurance",
        orchestrator_family="openai",
    )
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["provenance"]["observed_model"] == observed
    assert record["provenance"]["family"] == expected_family
    if catalogued:
        assert record["provenance"]["identity"] == "observed"
        assert "observed family anthropic inferred from catalogue" in record["provenance"]["notes"]
    assert not record["cross_family"]
    assert not record["certification_eligible"]


def test_catalogue_lookup_oserror_does_not_fail_finalisation(tmp_path, monkeypatch):
    exec_routing = importlib.import_module("skills.orchestrate.scripts.exec_routing")

    def unavailable():
        raise OSError("catalogue unavailable")

    monkeypatch.setattr(
        exec_routing,
        "_model_route_module",
        lambda: SimpleNamespace(load_catalog=unavailable),
    )
    code = """import json
for event in [
    {"type":"system","subtype":"init","model":"claude-haiku-4-5-20251001","session_id":"s-1"},
    {"type":"assistant","message":{"model":"claude-sonnet-5-5","content":[{"type":"text","text":"DONE"}]}},
    {"type":"result","result":"DONE","is_error":False,"session_id":"s-1"},
]:
    print(json.dumps(event), flush=True)
"""
    plan = fixture_plan(tmp_path, code, "claude")
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["provenance"]["family"] == "unknown"
    assert "observed model family unverified after substitution" in record["provenance"]["notes"]


def test_preface_env_and_credential_path_controls(tmp_path):
    plan = fixture_plan(
        tmp_path,
        "import os,json; print(json.dumps({k:os.environ.get(k) for k in ['PROVENANT_ROUTE','PROVENANT_RUN_ID','PROVENANT_CHAIR','AGENT_FABRIC_SEAT']}))",
        run_id="mcp-123",
        chair="chair-seat",
    )
    # Plain text is a supported compatibility response.
    plan["argv"][-1] = (
        "import os; print('|'.join(os.environ.get(k,'') for k in ['PROVENANT_ROUTE','PROVENANT_RUN_ID','PROVENANT_CHAIR','AGENT_FABRIC_SEAT']))"
    )
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert (
        tmp_path / "result.md"
    ).read_text().strip() == "codex/fixture@high|mcp-123|chair-seat|"
    assert plan["prompt"].startswith("You are codex/fixture@high via Fabric.")
    secrets = tmp_path / ".ssh"
    secrets.mkdir()
    assert str(secrets) not in fixture_plan(tmp_path, "", add_dirs=[secrets])['applied']['add_dirs']
    with pytest.raises(ValueError, match="read-only"):
        fixture_plan(tmp_path, "", sandbox="workspace-write")


@pytest.mark.parametrize("adapter", ["claude", "codex"])
def test_owner_contract_and_resume_keeps_same_run(tmp_path, adapter):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    executable = bindir / adapter
    executable.write_text("""#!/usr/bin/env python3
import json,sys
if sys.argv[1:3]==['debug','models']:
 print(json.dumps({'models':[{'slug':'gpt-6-luna','supported_reasoning_levels':[{'effort':'high'}]}]}));sys.exit()
prompt=sys.stdin.read()
if 'claude' in sys.argv[0]:
 print(json.dumps({'type':'system','subtype':'init','model':'claude-sonnet-4-6','session_id':'fixture-session'}))
 print(json.dumps({'type':'result','result':'DONE' if '--resume' in sys.argv else '```\\nQUESTION: Which branch?\\n```','is_error':False}))
else:
 print(json.dumps({'type':'thread.started','thread_id':'fixture-session'}))
 print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'DONE' if 'resume' in sys.argv else '```\\nQUESTION: Which branch?\\n```'}}))
 print(json.dumps({'type':'turn.completed'}))
""")
    executable.chmod(0o755)
    env = {
        **os.environ,
        "PATH": str(bindir) + ":" + os.environ["PATH"],
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
        "FABRIC_COOLDOWNS_PATH": str(tmp_path / "cooldowns.json"),
    }
    run = Path(
        subprocess.check_output(
            [
                str(SCRIPTS / "run_dir_init.sh"),
                "--kind",
                "dispatch",
                "--slug",
                "resume",
            ],
            cwd=tmp_path,
            text=True,
        ).strip()
    )
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Choose a branch")
    first = subprocess.run(
        [
            sys.executable,
            str(SCRIPTS / "dispatch_run.py"),
            "--run-dir",
            str(run),
            "--task-id",
            "task-1",
            "--tool",
            adapter,
            "--alias",
            "senior",
            "--role",
            "worker",
            "--prompt-file",
            str(prompt),
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
    )
    path = run / "tasks/task-1/attempt-001/attempt.json"
    assert path.exists(), first.stdout + first.stderr
    row = json.loads(path.read_text())
    assert row["schema"] == "fabric.attempt.v1"
    assert row["status"] == "input_required"
    assert row["session_id"] == "fixture-session"
    prompt.write_text("main")
    second = subprocess.run(
        [
            sys.executable,
            str(SCRIPTS / "dispatch_run.py"),
            "--run-dir",
            str(run),
            "--resume",
            row["run_id"],
            "--prompt-file",
            str(prompt),
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert second.returncode == 0, second.stdout + second.stderr
    row2 = json.loads((run / "tasks/task-1/attempt-002/attempt.json").read_text())
    assert row2["status"] == "ok"
    assert row2["run_id"] == row["run_id"]
    assert row2["session_id"] == "fixture-session"
    third = subprocess.run(
        [
            sys.executable,
            str(SCRIPTS / "dispatch_run.py"),
            "--run-dir",
            str(run),
            "--resume",
            row["run_id"],
            "--prompt-file",
            str(prompt),
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert third.returncode == 0, third.stdout + third.stderr
    row3 = json.loads((run / "tasks/task-1/attempt-003/attempt.json").read_text())
    assert row3["status"] == "ok" and row3["session_id"] == row2["session_id"]
    assert len((tmp_path / ".agent-run/runs/index.jsonl").read_text().splitlines()) == 3


def test_fallback_training_requires_explicit_opt_in(tmp_path):
    mod = importlib.import_module("skills.orchestrate.scripts.exec_routing")
    plan = fixture_plan(tmp_path, "")
    plan["requested_model"] = None
    plan["route"].update(
        alias="senior",
        candidates=["fixture", "paid", "training", "opencode/free-free"],
    )
    catalogue = {"models": {"training": {"trains_on_prompts": True}}, "adapters": {}}
    assert [item["model"] for item in mod.candidates(plan, True, catalogue)] == ["paid"]
    assert [item["model"] for item in mod.candidates(plan, "any", catalogue)] == [
        "paid",
        "training",
        "free-free",
    ]
    assert mod.candidates(plan, False, catalogue) == []


@pytest.mark.parametrize("explicit", [False, True])
def test_usage_limit_falls_back_as_attempt_two(tmp_path, explicit):
    # This checks routing, independently of the host's memory probe.
    policy = tmp_path / ".agents/fabric-policy.json"
    policy.parent.mkdir(parents=True, exist_ok=True)
    policy.write_text(json.dumps({"memory_floor_percent": {"read_only": 0}}))
    bindir = tmp_path / "bin"
    bindir.mkdir()
    cli = bindir / "claude"
    initial_model = "opus" if explicit else "sonnet"
    cli.write_text("""#!/usr/bin/env python3
import json,sys
sys.stdin.read()
model=sys.argv[sys.argv.index('--model')+1]
print(json.dumps({'type':'result','is_error':'TARGET_MODEL' in model,'result':"You've hit your usage limit" if 'TARGET_MODEL' in model else 'DONE'}))
sys.exit(1 if 'TARGET_MODEL' in model else 0)
""".replace("TARGET_MODEL", initial_model))
    cli.chmod(0o755)
    fallback_cli = bindir / 'opencode'
    fallback_cli.write_text("#!/usr/bin/env python3\nimport json\nprint(json.dumps({'type':'text','part':{'text':'DONE'}}))\n")
    fallback_cli.chmod(0o755)
    env = {
        **os.environ,
        "PROVENANT_NO_OS_CONFINEMENT": "1",
        "PATH": str(bindir) + ":" + os.environ["PATH"],
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
        "FABRIC_COOLDOWNS_PATH": str(tmp_path / "cooldowns.json"),
    }
    run = Path(
        subprocess.check_output(
            [str(SCRIPTS / "run_dir_init.sh"), "--kind", "dispatch"],
            cwd=tmp_path,
            text=True,
        ).strip()
    )
    prompt = tmp_path / "prompt.md"
    prompt.write_text("Reply DONE")
    result = subprocess.run(
        [
            sys.executable,
            str(SCRIPTS / "dispatch_run.py"),
            "--run-dir",
            str(run),
            "--tool",
            "claude",
            "--model" if explicit else "--alias",
            "opus" if explicit else "senior",
            "--fallback", "true",
            "--prompt-file",
            str(prompt),
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=15,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    rows = [
        json.loads(path.read_text())
        for path in sorted((run / "tasks").glob("*/attempt-*/attempt.json"))
    ]
    assert [row["status"] for row in rows] == ["usage_limited", "ok"]
    assert len({row["run_id"] for row in rows}) == 1
    assert rows[1]["provenance"]["fallback_from"]["status"] == "usage_limited"
    assert rows[0]["provenance"]["resolved_model"] != rows[1]["provenance"]["resolved_model"]
    assert json.loads((run / "RUN_RECEIPT.json").read_text())["status"] == "ok"
    cooldowns = json.loads((tmp_path / "cooldowns.json").read_text())["cooldowns"]
    (key, cooldown), = cooldowns.items()
    assert json.loads(key) == ["claude", rows[0]["account"], rows[0]["host"], "*"]
    assert (cooldown["account"], cooldown["host"]) == (rows[0]["account"], rows[0]["host"])
    assert cooldown["status"] == "usage_limited"
    assert cooldown["source_run"] == rows[0]["run_id"]


def test_codex_observed_model_comes_from_rollout_turn_context(tmp_path):
    sessions = tmp_path / "codex/sessions/2026/09/23"
    sessions.mkdir(parents=True)
    (sessions / "rollout-time-thread-123.jsonl").write_text(
        json.dumps({"type": "turn_context", "payload": {"model": "gpt-6-luna"}}) + "\n"
    )
    code = "import json; print(json.dumps({'type':'thread.started','thread_id':'thread-123'})); print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'OK'}})); print(json.dumps({'type':'turn.completed'}))"
    plan = fixture_plan(tmp_path, code)
    record = supervisor().execute(
        plan,
        tmp_path / "result.md",
        env={**os.environ, "CODEX_HOME": str(tmp_path / "codex")},
    )
    assert record["provenance"]["observed_model"] == "gpt-6-luna"
    assert record["provenance"]["observed_source"] == "codex:rollout.turn_context.model"
    assert record["provenance"]["identity"] == "observed"
    assert any("mismatch" in warning for warning in record["warnings"])


def test_opencode_observed_model_comes_from_export(tmp_path):
    child = tmp_path / "child"
    child.mkdir()
    pwd_file = tmp_path / "export-pwd"
    cli = tmp_path / "opencode"
    cli.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, pathlib, sys\n"
        "if sys.argv[1:3] == ['session', 'export'] and '--standalone' in sys.argv:\n"
        f"    pathlib.Path({str(pwd_file)!r}).write_text(os.environ['PWD'])\n"
        "    print(json.dumps({'info':{'model':{'providerID':'opencode-go','id':'deepseek-v4.1-flash','variant':'default'}}}))\n"
    )
    cli.chmod(0o755)
    plan = fixture_plan(
        tmp_path,
        "import json; print(json.dumps({'type':'text','sessionID':'oc-1','part':{'text':'OK'}}))",
        "opencode",
    )
    plan["cwd"] = str(child)
    record = supervisor().execute(
        plan,
        tmp_path / "result.md",
        env={**os.environ, "PATH": str(tmp_path) + ":" + os.environ["PATH"], "PWD": str(tmp_path)},
    )
    assert record["provenance"]["observed_model"] == "opencode-go/deepseek-v4.1-flash"
    assert record["provenance"]["observed_source"] == "opencode:session-export.info.model"
    assert pwd_file.read_text() == str(child)


def test_capture_is_bounded_but_result_is_complete(tmp_path, monkeypatch):
    module = supervisor()
    path = tmp_path / "events.jsonl"
    capture = module.BoundedCapture(path, limit=100)
    capture.write(b"A" * 80)
    capture.write(b"B" * 80)
    capture.close()
    assert path.read_bytes() == b"A" * 50 + b"B" * 50


def test_capture_rollover_keeps_output_timestamp_current_for_watchdog(tmp_path):
    module = supervisor()
    path = tmp_path / "events.jsonl"
    capture = module.BoundedCapture(path, limit=100)
    try:
        capture.write(b"A" * 100)
        os.utime(path, (time.time() - 600, time.time() - 600))
        capture.write(b"B" * 100)
        module.stall_watchdog.observe_if_silent(tmp_path, tmp_path, os.getpid(), True,
                                               "2000-01-01T00:00:00Z", 300)
        assert not (tmp_path / "watchdog.json").exists()
        assert path.stat().st_size == 100
    finally:
        capture.close()


@pytest.mark.parametrize("failure", ["write", "flush"])
def test_capture_keeps_tail_when_disk_writes_fail(tmp_path, failure):
    module = supervisor()
    capture = module.BoundedCapture(tmp_path / "events.jsonl", limit=100)
    stream = capture.file
    class FullDisk:
        def write(self, data):
            if failure == "write" or capture.total >= capture.limit:
                raise OSError(28, "No space left on device")
            return stream.write(data)
        def __getattr__(self, name):
            return getattr(stream, name)
    capture.file = FullDisk()
    try:
        capture.write(b"A" * 80)
        capture.write(b"B" * 80)
        assert capture.text() == "B" * 50
        assert capture.write_error.errno == 28
    finally:
        capture.close()


def test_handler_diagnostics_tolerate_enospc(tmp_path, monkeypatch):
    module = supervisor()
    plan = fixture_plan(tmp_path, "")
    plan["argv"] = [str(tmp_path / "missing-provider")]
    original_fdopen = os.fdopen
    class FullDisk:
        def __init__(self, stream):
            self.stream = stream
        def write(self, data):
            raise OSError(28, "No space left on device")
        def __getattr__(self, name):
            return getattr(self.stream, name)
    def fdopen(fd, mode="r", *args, **kwargs):
        linked = os.fstat(fd).st_nlink > 0
        stream = original_fdopen(fd, mode, *args, **kwargs)
        return FullDisk(stream) if mode == "w+b" and linked else stream
    monkeypatch.setattr(module.os, "fdopen", fdopen)
    record = module.execute(plan, tmp_path / "result.md")
    assert record["status"] == "tool_missing"
    assert "missing-provider" in record["evidence"]["excerpt"]
    assert any("capture write failed: [Errno 28]" in warning for warning in record["warnings"])


def codex_profile(plan):
    """The permissions profile a Codex plan selects, and the config overrides that build it."""
    argv = plan["argv"]
    selected = [argv[index + 1] for index, arg in enumerate(argv) if arg == "-c"
                and argv[index + 1].startswith("default_permissions=")]
    assert len(selected) == 1, argv
    name = json.loads(selected[0].split("=", 1)[1])
    overrides = [argv[index + 1] for index, arg in enumerate(argv) if arg == "-c"
                 and argv[index + 1].startswith("permissions.")]
    assert all(item.startswith("permissions." + name + ".") for item in overrides), overrides
    return name, overrides


def provenant_state():
    """Provenant's own state directory, which a Codex lane may write for lock files."""
    return str(Path(os.environ["XDG_STATE_HOME"]) / "provenant")


def codex_filesystem(plan):
    """The filesystem table of the Codex permissions profile a writer plan passes."""
    name, overrides = codex_profile(plan)
    prefix = "permissions." + name + ".filesystem="
    value = next(item for item in overrides if item.startswith(prefix))
    return {Path(path): access for path, access in tomllib.loads("t = " + value[len(prefix):])["t"].items()}


def codex_access(table, path):
    """The access Codex applies to a path: the nearest enclosing table entry decides."""
    entries = [entry for entry in table if path == entry or path.is_relative_to(entry)]
    return table[max(entries, key=lambda entry: len(entry.parts))] if entries else None


def nested_lanes(tmp_path):
    """This repository's layout: linked worktrees nested at <repo>/.worktrees/<name>."""
    repo = tmp_path / "repo"
    git_repo(repo, branch="main")
    lane = make_linked_worktree(repo, "lane", path=repo / ".worktrees/lane")
    other = make_linked_worktree(repo, "other", path=repo / ".worktrees/other")
    return repo, lane, other


def codex_writer_plan(lane, workspace, resume=None, **controls):
    return supervisor().build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", mode="worktree_write",
        worktree=lane, workspace_root=workspace, resume_session=resume, **controls,
    )


@pytest.mark.parametrize("capabilities", [[], ["postgres"]], ids=["native", "postgres"])
@pytest.mark.parametrize("resume", [None, "thread-1"], ids=["fresh", "resume"])
def test_codex_writer_grants_only_fabric_database_and_runs(monkeypatch, tmp_path, capabilities, resume):
    mod = supervisor()
    repo, lane, _ = nested_lanes(tmp_path)
    state = tmp_path / "fabric-state"
    monkeypatch.setenv("AGENT_FABRIC_STATE_DIRECTORY", str(state))
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path / "catalogue"))
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    plan = codex_writer_plan(lane, repo, resume, capabilities=capabilities)
    files = [state / ("fabric.sqlite3" + suffix) for suffix in ("", "-wal", "-shm", "-journal")]
    runs = repo / ".agent-run/runs"
    denied = [repo / ".agent-run/sessions/DIRECTION.md", state / "capabilities.json",
              state / "cooldowns.json", state / "route-health.json", tmp_path / "catalogue",
              mod.Path.home() / ".config/gh", mod.Path.home() / ".local/share/pnpm/store"]
    if capabilities:
        profile = mod.provider_confinement.os_confinement_profile(plan)
        grants = plan["applied"]["write_boundary"]["writable_paths"]
        assert f'(subpath "{runs}")' in profile
        for path in files:
            assert f'(allow file-write* (literal "{path}"))' in profile
        for path in denied:
            assert not any(path == Path(root) or path.is_relative_to(Path(root)) for root in grants)
        sockets = next(line for line in profile.splitlines() if line.startswith("(allow network-outbound"))
        assert str(state) not in sockets and str(runs) not in sockets
    else:
        table = codex_filesystem(plan)
        for path in [*files, runs / "nested/receipt.json"]:
            assert codex_access(table, path) == "write", path
        for path in denied:
            assert codex_access(table, path) != "write", path
    read = mod.build_plan("codex", {}, "hello", cwd=lane, workspace_root=repo)
    assert str(state) not in " ".join(read["argv"])


@pytest.mark.parametrize("target", ["credential", "home", "ancestor", "git", "git-link", "session-link", "home-link"])
def test_codex_shared_paths_apply_add_dir_safety_checks(monkeypatch, tmp_path, target):
    mod = supervisor()
    repo, lane, _ = nested_lanes(tmp_path)
    home = tmp_path / "home"
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    state = {"credential": home / ".ssh", "home": home, "ancestor": tmp_path,
             "git": repo / ".git/hooks"}.get(target, tmp_path / "fabric-state")
    if target.endswith("-link"):
        state.mkdir()
        destination = {"git-link": repo / ".git/config",
                       "session-link": repo / ".agent-run/sessions/DIRECTION.md",
                       "home-link": home / ".zshrc"}[target]
        (state / "fabric.sqlite3").symlink_to(destination)
    monkeypatch.setenv("AGENT_FABRIC_STATE_DIRECTORY", str(state))
    plan = codex_writer_plan(lane, repo)
    assert codex_access(codex_filesystem(plan), state / "fabric.sqlite3") != "write"


def test_codex_shared_runs_link_does_not_grant_sessions(tmp_path):
    repo, lane, _ = nested_lanes(tmp_path)
    run_root = repo / ".agent-run"
    sessions = run_root / "sessions"
    sessions.mkdir(parents=True)
    (run_root / "runs").symlink_to(sessions, target_is_directory=True)
    plan = codex_writer_plan(lane, repo)
    assert codex_access(codex_filesystem(plan), sessions / "DIRECTION.md") != "write"


@pytest.mark.parametrize("setting", ["", "~/state"])
def test_codex_shared_database_path_matches_fabric_resolution(monkeypatch, tmp_path, setting):
    repo, lane, _ = nested_lanes(tmp_path)
    monkeypatch.setenv("AGENT_FABRIC_STATE_DIRECTORY", setting)
    plan = codex_writer_plan(lane, repo)
    # Fabric's Node path.resolve treats empty and tilde values as cwd-relative.
    assert codex_filesystem(plan)[lane / setting / "fabric.sqlite3"] == "write"


@pytest.mark.parametrize("resume", [None, "thread-1"], ids=["fresh", "resume"])
def test_codex_writer_gets_the_wrapped_writer_git_boundary(tmp_path, resume):
    repo, lane, other = nested_lanes(tmp_path)
    common = Path(git(lane, "rev-parse", "--path-format=absolute", "--git-common-dir").strip()).resolve()
    private = Path(git(lane, "rev-parse", "--absolute-git-dir").strip()).resolve()
    sibling = Path(git(other, "rev-parse", "--absolute-git-dir").strip()).resolve()

    plan = codex_writer_plan(lane, tmp_path, resume, network=False, add_dirs=[str(common)])

    argv = plan["argv"]
    name, overrides = codex_profile(plan)
    table = codex_filesystem(plan)
    add_dirs = {Path(path) for path in plan["applied"]["add_dirs"]}
    add_dirs |= {Path(argv[index + 1]) for index, arg in enumerate(argv) if arg == "--add-dir"}
    assert f"permissions.{name}.extends=\":workspace\"" in overrides
    assert f"permissions.{name}.network.enabled=false" in overrides
    assert "-s" not in argv and not any("sandbox_mode" in arg or "sandbox_workspace_write" in arg for arg in argv)
    assert table.pop(Path(provenant_state())) == "write"  # Provenant's own lock directory, beside the planned boundary
    assert plan["applied"]["write_boundary"]["filesystem"] == {str(path): access for path, access in table.items()}
    for granted in (private, private / "index", private / "index.lock", private / "HEAD",
                    private / "ORIG_HEAD", private / "logs/HEAD",
                    common / "objects", common / "refs/heads/lane", common / "refs/heads/lane.lock",
                    common / "logs/refs/heads/lane"):
        assert codex_access(table, granted) == "write", granted
    for denied in (common, common / "hooks", common / "hooks/pre-commit", common / "config",
                   common / "info/exclude", common / "worktrees", sibling, sibling / "HEAD",
                   private / "config.worktree", private / "commondir", private / "gitdir", lane / ".git",
                   common / "objects/info", common / "objects/info/alternates", common / "refs/heads/main",
                   common / "refs/remotes/origin/main", common / "refs/tags/v1", common / "packed-refs",
                   common / "logs/refs/heads/main"):
        assert codex_access(table, denied) == "read", denied
        assert not any(denied == root or denied.is_relative_to(root) for root in add_dirs), denied
    assert table[lane.resolve() / ".git"] == "read"
    assert common not in add_dirs
    assert any("drops Git common directory add-dir" in warning for warning in plan["warnings"])
    assert "--ephemeral" not in argv


def test_codex_permissions_profile_name_is_unique_to_each_plan(tmp_path):
    # Codex merges config tables, so a fixed name would inherit grants a system config adds under it.
    _, lane, _ = nested_lanes(tmp_path)
    names = {codex_profile(codex_writer_plan(lane, tmp_path, resume))[0] for resume in (None, None, "thread-1")}
    names.add(codex_profile(supervisor().build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", cwd=tmp_path, network=True))[0])
    assert len(names) == 4
    assert all(re.fullmatch(r"provenant-[0-9a-f]{32}", name) for name in names), names


@pytest.mark.live_provider
@pytest.mark.skipif(sys.platform != "darwin" or shutil.which("codex") is None,
                    reason="needs the codex CLI's macOS sandbox")
@pytest.mark.parametrize("resume", [None, "thread-1"], ids=["fresh", "resume"])
def test_codex_sandbox_commits_in_nested_lane_and_ignores_inherited_grants(tmp_path, resume):
    """Run the generated policy with `codex sandbox` (no model call) against a config that
    grants the common hooks and config under the profile names Provenant has used."""
    _, lane, other = nested_lanes(tmp_path)
    common = Path(git(lane, "rev-parse", "--path-format=absolute", "--git-common-dir").strip()).resolve()
    plan = codex_writer_plan(lane, tmp_path, resume, network=False)
    name, overrides = codex_profile(plan)
    codex_home = tmp_path / "codex-home"
    codex_home.mkdir()
    (codex_home / "config.toml").write_text("".join(
        f'[permissions.{inherited}.filesystem]\n{json.dumps(str(common / "hooks"))} = "write"\n'
        f'{json.dumps(str(common / "config"))} = "write"\n'
        for inherited in ("provenant-worktree-write", "provenant-read-only", "provenant-read-only-network")))
    command = ["codex", "sandbox", "-P", name, "-C", str(lane)]
    for item in overrides:
        command += ["-c", item]
    probe = (f"echo a > f && git add f && git {' '.join(GIT_FIXTURE)} commit -q -m lane && echo commit=ok; "
             f"echo x > {common / 'hooks/pre-commit'} || echo hooks=denied; "
             f"git config --file {common / 'config'} probe.key 1 || echo config=denied; "
             f"echo x > {common / 'worktrees' / other.name / 'probe'} || echo sibling=denied; "
             f"echo x >> {lane / '.git'} || echo marker=denied")
    result = subprocess.run([*command, "--", "/bin/sh", "-c", probe], cwd=lane, capture_output=True,
                            text=True, timeout=60, env={**os.environ, "CODEX_HOME": str(codex_home)})
    if "commit=ok" not in result.stdout and "sandbox_apply" in result.stderr:
        pytest.skip("macOS refuses a nested sandbox here")
    assert result.stdout.split() == ["commit=ok", "hooks=denied", "config=denied", "sibling=denied",
                                     "marker=denied"], result.stdout + result.stderr
    assert git(lane, "log", "-1", "--format=%s").strip() == "lane"
    assert not (common / "hooks/pre-commit").exists()


def test_writer_attempt_disables_git_writes_to_the_common_directory(tmp_path):
    code = """import json, os
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':json.dumps({key: value for key, value in os.environ.items() if key.startswith('GIT_CONFIG')})}}))
print(json.dumps({'type':'turn.completed'}))
"""
    plan = fixture_plan(tmp_path, code, mode="worktree_write")
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok", record
    assert json.loads((tmp_path / "result.md").read_text()) == {
        "GIT_CONFIG_COUNT": "3",
        "GIT_CONFIG_KEY_0": "gc.auto", "GIT_CONFIG_VALUE_0": "0",
        "GIT_CONFIG_KEY_1": "maintenance.auto", "GIT_CONFIG_VALUE_1": "false",
        "GIT_CONFIG_KEY_2": "rerere.enabled", "GIT_CONFIG_VALUE_2": "false",
    }


GIT_FIXTURE = ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
               "-c", "commit.gpgsign=false"]
SKILL = ".agents/skills/fixture/SKILL.md"


def git(root, *args):
    return subprocess.run(["git", "-C", str(root), *GIT_FIXTURE, *args],
                          check=True, capture_output=True, text=True).stdout


def instruction_lane(tmp_path):
    """A primary on main with a tracked skill, and a lane worktree cut before main moves."""
    repo = tmp_path / "repo"
    git_repo(repo, branch="main", files={SKILL: "v1\n", "app.txt": "app\n"}, commit=False)
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "initial")
    lane = repo / ".worktrees/lane"
    make_linked_worktree(repo, "lane", path=lane)
    (lane / "app.txt").write_text("lane\n")
    git(lane, "commit", "-q", "-am", "lane work")
    (repo / SKILL).write_text("v2\n")
    git(repo, "commit", "-q", "-am", "main updates the skill")
    return repo, lane


def lane_attempt(tmp_path, lane, script, policy=None, adapter="codex", **execution):
    done = ("print('{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"done\"}}')\n"
            "print('{\"type\":\"turn.completed\"}')\n") if adapter == "codex" else (
            "print('{\"type\":\"result\",\"result\":\"done\",\"is_error\":false,\"session_id\":\"s-1\"}')\n")
    code = ("import os, subprocess\n"
            f"def git(*args): return subprocess.run(['git', *{GIT_FIXTURE!r}, *args], check=True, "
            "capture_output=True, text=True).stdout.strip()\n"
            + script + done)
    run_dir = tmp_path / "attempt"
    run_dir.mkdir()
    if policy is not None:
        # The plan's workspace root holds the project policy.
        (run_dir / ".agents").mkdir()
        (run_dir / ".agents/fabric-policy.json").write_text(json.dumps(policy))
    plan = fixture_plan(run_dir, code, adapter, mode="worktree_write", worktree=lane, run_dir=run_dir)
    return supervisor().execute(plan, run_dir / "result.md", **execution)


def test_codex_writer_may_write_worktree_instructions(tmp_path):
    _, lane = instruction_lane(tmp_path)
    codex = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello",
                                    mode="worktree_write", worktree=lane, workspace_root=tmp_path)
    claude = supervisor().build_plan("claude", {"resolved_model": "fixture"}, "hello",
                                     mode="worktree_write", worktree=lane, workspace_root=tmp_path)
    assert str(lane / ".agents") in codex["applied"]["add_dirs"]
    assert codex_filesystem(codex)[lane / ".agents"] == "write"
    assert str(lane / ".agents") not in claude["applied"]["add_dirs"]


@pytest.mark.parametrize("refresh", ["git('merge', '-q', '--no-edit', 'main')\n", "git('rebase', '-q', 'main')\n"],
                         ids=["merge", "rebase"])
def test_codex_writer_refreshing_from_integration_branch_passes(tmp_path, refresh):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, refresh)
    assert record["status"] == "ok", record
    assert "error" not in record
    assert (lane / SKILL).read_text() == "v2\n"


@pytest.mark.parametrize("fetched", [True, False], ids=["upstream", "unfetched-upstream"])
def test_codex_writer_refreshing_from_upstream_passes(tmp_path, fetched):
    repo, lane = instruction_lane(tmp_path)
    git(repo, "remote", "add", "origin", str(tmp_path / "origin.git"))
    git(repo, "config", "branch.main.remote", "origin")
    git(repo, "config", "branch.main.merge", "refs/heads/main")
    source = "main"
    if fetched:
        git(repo, "update-ref", "refs/remotes/origin/main", "main")
        git(repo, "reset", "-q", "--hard", "main~1")
        source = "origin/main"
    record = lane_attempt(tmp_path, lane, f"git('merge', '-q', '--no-edit', {source!r})\n")
    assert record["status"] == "ok", record


def test_codex_writer_keeps_changes_its_branch_already_carried(tmp_path):
    _, lane = instruction_lane(tmp_path)
    (lane / SKILL).write_text("branch edit\n")
    git(lane, "commit", "-q", "-am", "reviewed skill edit")
    record = lane_attempt(tmp_path, lane, "")
    assert record["status"] == "ok", record


@pytest.mark.parametrize("edit", [
    f"open({SKILL!r}, 'w').write('lane edit\\n')\n",
    f"open({SKILL!r}, 'w').write('lane edit\\n'); git('commit', '-q', '-am', 'edit')\n",
    "open('.agents/skills/fixture/extra.md', 'w').write('new\\n')\n",
    "common = git('rev-parse', '--path-format=absolute', '--git-common-dir')\n"
    "open(common + '/info/exclude', 'a').write('.agents/skills/fixture/extra.md\\n')\n"
    "open('.agents/skills/fixture/extra.md', 'w').write('new\\n')\n",
    f"git('update-index', '--skip-worktree', {SKILL!r}); open({SKILL!r}, 'w').write('lane edit\\n')\n",
    "blob = subprocess.run(['git', 'hash-object', '-w', '--stdin'], input='x', capture_output=True, "
    "text=True, check=True).stdout.strip()\n"
    "git('update-index', '--add', '--cacheinfo', '100644,' + blob + ',.Agents/skills/fixture/SKILL.md')\n"
    "git('commit', '-q', '-m', 'plumbing')\n",
    "os.mkfifo('.agents/skills/fixture/pipe')\n",
], ids=["uncommitted", "committed", "untracked", "ignored", "skip-worktree", "case-variant", "fifo"])
def test_codex_writer_authoring_instructions_fails_under_deny_policy(tmp_path, edit):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, edit, policy={"instruction_changes": "deny"})
    assert record["status"] == "failed"
    assert record["landable"] is False
    assert record["quarantine"]["restored"] is False
    assert "instruction edits remain on the lane branch" in record["fix"]
    assert (tmp_path / "attempt/protected.patch").is_file()
    assert str(tmp_path / "attempt/protected.patch") in record["quarantine"]["patches"]
    assert Path(record["output_path"]).read_text() == "done"
    assert record["error"] == record["evidence"]["signature"] == (
        "protected_instructions_unverifiable" if "pipe" in edit else "protected_instructions_changed")
    assert any(".agents/skills/fixture/" in warning.casefold() for warning in record["warnings"])


@pytest.mark.parametrize("edit, expected", [
    (f"open({SKILL!r}, 'w').write('lane edit\\n')\n", {SKILL: "lane edit\n"}),
    (f"open({SKILL!r}, 'w').write('lane edit\\n'); git('commit', '-q', '-am', 'edit')\n", {SKILL: "lane edit\n"}),
    ("open('.agents/skills/fixture/extra.md', 'w').write('new\\n')\n", {".agents/skills/fixture/extra.md": "new\n"}),
    ("common = git('rev-parse', '--path-format=absolute', '--git-common-dir')\n"
     "open(common + '/info/exclude', 'a').write('.agents/skills/fixture/extra.md\\n')\n"
     "open('.agents/skills/fixture/extra.md', 'w').write('new\\n')\n", {".agents/skills/fixture/extra.md": "new\n"}),
    (f"git('update-index', '--skip-worktree', {SKILL!r}); open({SKILL!r}, 'w').write('lane edit\\n')\n",
     {SKILL: "lane edit\n"}),
    ("import shutil; shutil.rmtree('.agents')\n", {SKILL: None}),
], ids=["uncommitted", "committed", "untracked", "ignored", "skip-worktree", "deleted"])
def test_codex_writer_instruction_changes_are_quarantined_by_default(tmp_path, edit, expected):
    _, lane = instruction_lane(tmp_path)
    start = git(lane, "rev-parse", "HEAD").strip()
    record = lane_attempt(tmp_path, lane, edit)
    patch = tmp_path / "attempt" / "protected.patch"
    assert record["status"] == "quarantined", record
    assert record["exit"] == 0
    assert record["landable"] is False
    assert record["retryable"] is False
    assert record["quarantine"]["restored"] is True
    assert str(patch) in record["quarantine"]["patches"]
    assert Path(record["output_path"]).read_text() == "done"
    assert record["certification_eligible"] is False
    assert "error" not in record
    assert any("quarantined" in warning and str(patch) in warning for warning in record["warnings"]), record["warnings"]
    # The lane's result no longer carries the change: HEAD, index and files match its start.
    assert (lane / SKILL).read_text() == "v1\n"
    assert not (lane / ".agents/skills/fixture/extra.md").exists()
    assert git(lane, "diff", "--name-only", start, "--", ".agents") == ""
    assert git(lane, "diff", "--cached", "--name-only", "--", ".agents") == ""
    # The patch carries the change, for the chair to apply deliberately.
    check = tmp_path / "check"
    git(lane, "worktree", "add", "-q", "--detach", str(check), start)
    git(check, "apply", str(patch))
    for path, content in expected.items():
        assert ((check / path).read_text() if content is not None else (check / path).exists()) == (
            content if content is not None else False)


def test_quarantine_keeps_the_integration_branch_version_a_lane_merged(tmp_path):
    repo, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, "git('merge', '-q', '--no-edit', 'main')\n"
                          f"open({SKILL!r}, 'w').write('v2 lane\\n'); git('commit', '-q', '-am', 'edit')\n")
    assert record["status"] == "quarantined", record
    assert (lane / SKILL).read_text() == "v2\n"
    git(repo, "apply", str(tmp_path / "attempt" / "protected.patch"))
    assert (repo / SKILL).read_text() == "v2 lane\n"


def test_quarantine_never_follows_a_planted_patch_link(tmp_path):
    _, lane = instruction_lane(tmp_path)
    outside = tmp_path / "outside.txt"
    patch = tmp_path / "attempt" / "protected.patch"
    record = lane_attempt(tmp_path, lane, f"os.symlink({str(outside)!r}, {str(patch)!r})\n"
                          f"open({SKILL!r}, 'w').write('lane edit\\n')\n")
    assert not outside.exists()
    assert record["status"] == "quarantined", record
    assert not patch.is_symlink() and "lane edit" in patch.read_text()


def test_quarantine_restores_each_starting_view(tmp_path):
    _, lane = instruction_lane(tmp_path)
    notes = ".agents/skills/fixture/notes.md"
    (lane / notes).write_text("start notes\n")  # untracked at the start
    (lane / SKILL).write_text("staged start\n")
    git(lane, "add", SKILL)
    (lane / SKILL).write_text("disk start\n")  # staged and unstaged edits at the start
    record = lane_attempt(tmp_path, lane, f"open({notes!r}, 'w').write('lane notes\\n')\n"
                          f"open({SKILL!r}, 'w').write('lane edit\\n'); git('add', {SKILL!r})\n")
    assert record["status"] == "quarantined", record
    assert (lane / notes).read_text() == "start notes\n"
    assert (lane / SKILL).read_text() == "disk start\n"
    assert git(lane, "show", ":" + SKILL) == "staged start\n"
    assert git(lane, "show", "HEAD:" + SKILL) == "v1\n"
    git(lane, "apply", str(tmp_path / "attempt" / "protected.patch"))
    assert (lane / notes).read_text() == "lane notes\n"
    assert (lane / SKILL).read_text() == "lane edit\n"


@pytest.mark.parametrize("script", [
    "os.remove('.agents/local/notes.md')\n",
    "import shutil; shutil.rmtree('.agents/local'); os.symlink('/tmp', '.agents/local')\n",
], ids=["removed", "link-swapped"])
def test_quarantine_restores_and_reports_an_untracked_start_file_the_lane_removed(tmp_path, script):
    _, lane = instruction_lane(tmp_path)
    notes = ".agents/local/notes.md"
    (lane / notes).parent.mkdir()
    (lane / notes).write_text("start notes\n")  # untracked at the start
    record = lane_attempt(tmp_path, lane, script)
    assert record["status"] == "quarantined", record
    assert not (lane / ".agents/local").is_symlink()
    assert (lane / notes).read_text() == "start notes\n"
    assert any("quarantined" in warning and notes in warning for warning in record["warnings"]), record["warnings"]


def test_quarantine_archives_a_staged_change_the_disk_no_longer_shows(tmp_path):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, f"open({SKILL!r}, 'w').write('staged lane\\n'); git('add', {SKILL!r})\n"
                          f"open({SKILL!r}, 'w').write('v1\\n')\n")
    assert record["status"] == "quarantined", record
    assert git(lane, "show", ":" + SKILL) == "v1\n"
    archived = [warning for warning in record["warnings"] if "quarantined" in warning]
    assert archived and "protected.index.patch" in archived[0], record["warnings"]
    assert "staged lane" in (tmp_path / "attempt" / "protected.index.patch").read_text()
    assert (tmp_path / "attempt" / "protected.patch").read_bytes() == b""
    assert len(record["quarantine"]["patches"]) == 2


@pytest.mark.parametrize("policy, status", [(None, "quarantined"), ({"instruction_changes": "deny"}, "failed")])
def test_a_merge_that_discards_integration_instruction_changes_is_caught(tmp_path, policy, status):
    _, lane = instruction_lane(tmp_path)
    git(lane, "reset", "-q", "--hard", "HEAD~1")  # the lane starts from main's old commit
    record = lane_attempt(tmp_path, lane, "git('merge', '-q', '-s', 'ours', '--no-edit', 'main')\n", policy=policy)
    assert record["status"] == status, record
    if policy is None:
        assert (lane / SKILL).read_text() == "v2\n"
        assert git(lane, "show", "HEAD:" + SKILL) == "v2\n"
    else:
        assert record["error"] == "protected_instructions_changed"


def test_a_merge_that_keeps_the_lanes_own_earlier_change_to_a_path_both_sides_changed_passes(tmp_path):
    _, lane = instruction_lane(tmp_path)
    (lane / SKILL).write_text("lane v\n")
    git(lane, "commit", "-q", "-am", "the lane's branch changed the skill before this attempt")
    # Both sides changed the path since their merge base, so the start's version stands.
    record = lane_attempt(tmp_path, lane, "git('merge', '-q', '-s', 'ours', '--no-edit', 'main')\n",
                          policy={"instruction_changes": "deny"})
    assert record["status"] == "ok", record
    assert not any("quarantined" in warning for warning in record["warnings"])
    assert git(lane, "show", "HEAD:" + SKILL) == "lane v\n"


def test_other_writer_adapters_have_instruction_changes_quarantined(tmp_path):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, f"open({SKILL!r}, 'w').write('lane edit\\n')\n", adapter="claude")
    assert record["status"] == "quarantined", record
    assert (lane / SKILL).read_text() == "v1\n"
    assert any("quarantined" in warning for warning in record["warnings"])


def test_codex_writer_instruction_changes_pass_under_allow_policy(tmp_path):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, f"open({SKILL!r}, 'w').write('lane edit\\n')\n",
                          policy={"instruction_changes": "allow"})
    assert record["status"] == "ok", record
    assert (lane / SKILL).read_text() == "lane edit\n"
    assert any("allowed by policy" in warning and SKILL in warning for warning in record["warnings"])
    assert not (tmp_path / "attempt" / "protected.patch").exists()


def test_unknown_instruction_policy_warns_and_quarantines(tmp_path):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, f"open({SKILL!r}, 'w').write('lane edit\\n')\n",
                          policy={"instruction_changes": "alow"})
    assert record["status"] == "quarantined", record
    assert (lane / SKILL).read_text() == "v1\n"
    assert any("instruction_changes" in warning for warning in record["warnings"])


@pytest.mark.parametrize("policy", [None, {"instruction_changes": "allow"}], ids=["default", "allow"])
def test_special_instruction_changes_fail_a_successful_result(tmp_path, policy):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, "os.mkfifo('.agents/skills/fixture/pipe')\n", policy=policy)
    assert record["status"] == "failed"
    assert "instruction edits remain on the lane branch" in record["fix"]
    assert record["error"] == "protected_instructions_unverifiable"
    assert record["landable"] is False
    assert record["quarantine"]["patch_error"]
    assert Path(record["output_path"]).read_text() == "done"


def test_codex_writer_may_run_instruction_scripts(tmp_path):
    _, lane = instruction_lane(tmp_path)
    (lane / ".agents/skills/fixture/tool.py").write_text("VALUE = 1\n")
    git(lane, "add", ".")
    git(lane, "commit", "-q", "-m", "skill tool")
    record = lane_attempt(tmp_path, lane, "subprocess.run(['python3', '-c', 'import sys; "
                          "sys.path.insert(0, \".agents/skills/fixture\"); import tool'], check=True)\n")
    assert record["status"] == "ok", record


def test_codex_writer_committing_an_instruction_root_entry_fails(tmp_path):
    _, lane = instruction_lane(tmp_path)
    git(lane, "rm", "-r", "-q", "--cached", ".agents")
    git(lane, "commit", "-q", "-m", "untrack instructions")
    record = lane_attempt(tmp_path, lane, (
        "blob = subprocess.run(['git', 'hash-object', '-w', '--stdin'], input='/elsewhere', "
        "capture_output=True, text=True, check=True).stdout.strip()\n"
        "git('update-index', '--add', '--cacheinfo', '120000,' + blob + ',.agents')\n"
        "git('commit', '-q', '-m', 'link instructions')\n"))
    assert record["error"] == "protected_instructions_changed"
    assert any(warning.endswith(": .agents") for warning in record["warnings"])


def test_codex_writer_unresolved_instruction_conflict_fails_with_instruction_changes(tmp_path):
    _, lane = instruction_lane(tmp_path)
    (lane / SKILL).write_text("branch edit\n")
    git(lane, "commit", "-q", "-am", "reviewed skill edit")
    record = lane_attempt(tmp_path, lane, f"subprocess.run(['git', *{GIT_FIXTURE!r}, 'merge', '-q', 'main'])\n")
    assert record["status"] == "failed"
    assert record["error"] == "protected_instructions_changed"
    assert any(SKILL + " (unresolved conflict)" in warning for warning in record["warnings"])


@pytest.mark.parametrize("script", [
    "os.makedirs('.agents/hidden'); open('.agents/hidden/x', 'w').write('x'); os.chmod('.agents/hidden', 0)\n",
    "import shutil; shutil.copytree('.agents', '../elsewhere'); shutil.rmtree('.agents'); "
    "os.symlink('../elsewhere', '.agents')\n",
    "open('.agents/big', 'wb').truncate(1 << 40)\n",
], ids=["unreadable", "root-symlink", "oversized"])
def test_codex_writer_unverifiable_instructions_fail_a_successful_result(tmp_path, script):
    _, lane = instruction_lane(tmp_path)
    hidden = lane / ".agents" / "hidden"
    try:
        record = lane_attempt(tmp_path, lane, script)
    finally:
        if hidden.exists():
            hidden.chmod(0o755)
    assert record["status"] == "failed"
    assert record["error"] == "protected_instructions_unverifiable"
    assert record["landable"] is False
    assert any("unverifiable" in warning for warning in record["warnings"])


@pytest.mark.parametrize("edit", ["", f"open({SKILL!r}, 'w').write('lane edit\\n')\n",
                                 f"open({SKILL!r}, 'w').write('lane edit\\n'); git('commit', '-q', '-am', 'edit')\n"],
                         ids=["unchanged", "uncommitted", "committed"])
def test_instruction_check_verifies_git_views_with_spared_processes(tmp_path, monkeypatch, edit):
    module = supervisor()
    original = module._Descendants.stop

    def stop_sparing_a_process(self, *args, **kwargs):
        reaped = original(self, *args, **kwargs)
        self.spared_at_stop.add((999999, "fixture"))
        return reaped

    monkeypatch.setattr(module._Descendants, "stop", stop_sparing_a_process)
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, edit, policy={"instruction_changes": "deny"})
    assert record["spared"] == 1
    if edit:
        assert record["error"] == "protected_instructions_changed"
    else:
        assert record["status"] == "ok", record
        assert "error" not in record
    assert not any("left running" in warning for warning in record["warnings"])


@pytest.mark.parametrize("edit", ["", f"open({SKILL!r}, 'w').write('lane edit\\n')\n",
                                 f"open({SKILL!r}, 'w').write('lane edit\\n'); git('commit', '-q', '-am', 'edit')\n"],
                         ids=["unchanged", "uncommitted", "committed"])
def test_signalled_writer_checks_instructions_with_spared_descendants(tmp_path, monkeypatch, edit):
    module = supervisor()
    original = module._Descendants.stop

    def stop_sparing_a_process(self, *args, **kwargs):
        reaped = original(self, *args, **kwargs)
        self.spared_at_stop.add((999999, "fixture"))
        return reaped

    monkeypatch.setattr(module._Descendants, "stop", stop_sparing_a_process)
    _, lane = instruction_lane(tmp_path)
    provider, signalled = [], []
    ready = tmp_path / "provider-ready"

    def interrupt(_timestamp):
        if ready.exists() and not signalled:
            signalled.append(True)
            os.kill(provider[0].pid, signal.SIGTERM)

    record = lane_attempt(tmp_path, lane, edit + f"open({str(ready)!r}, 'w').close()\nimport time\nprint('ready', flush=True)\ntime.sleep(60)\n",
                          on_start=provider.append, on_progress=interrupt)
    assert record["spared"] == 1
    assert record["evidence"]["signal"] == signal.SIGTERM
    if edit:
        assert record["status"] == "failed"
        assert record["error"] == "protected_instructions_changed"
    else:
        assert record["status"] == "failed", record
        assert "error" not in record
    assert not any("left running" in warning for warning in record["warnings"])


def test_instruction_check_runs_no_repository_hooks(tmp_path):
    _, lane = instruction_lane(tmp_path)
    marker = tmp_path / "hook-ran"
    hook = tmp_path / "fsmonitor.sh"
    hook.write_text(f"#!/bin/sh\ntouch {marker}\n")
    hook.chmod(0o755)
    record = lane_attempt(tmp_path, lane, (
        f"git('config', 'core.fsmonitor', {str(hook)!r})\n"
        "git('config', 'core.repositoryformatversion', '1')\n"
        "git('config', 'extensions.partialClone', 'origin')\n"
        "git('config', 'remote.origin.url', 'ssh://example.invalid/repo')\n"
        "git('config', 'remote.origin.promisor', 'true')\n"
        f"git('config', 'core.sshCommand', {str(hook)!r})\n"
        "common = git('rev-parse', '--path-format=absolute', '--git-common-dir')\n"
        "open(common + '/refs/heads/main', 'w').write('1' * 40 + '\\n')\n"))
    assert record["status"] == "ok", record
    assert not marker.exists()


def test_plan_only_honors_read_only_cwd_inside_workspace(tmp_path):
    child = tmp_path / "nested"
    child.mkdir()
    cli = tmp_path / "claude"
    cli.write_text("#!/bin/sh\nexit 99\n")
    cli.chmod(0o755)
    env = {
        **os.environ,
        "PATH": str(tmp_path) + ":" + os.environ["PATH"],
        "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT),
        "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
    }
    result = subprocess.run(
        [
            str(SCRIPTS / "cf_dispatch.py"),
            "--tool",
            "claude",
            "--intent",
            "ordinary",
            "--prompt",
            "hello",
            "--cwd",
            str(child),
            "--plan-only",
        ],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0, result.stderr + result.stdout
    assert json.loads(result.stdout)["cwd"] == str(child)


def test_opencode_v2_argv_is_standalone_and_has_no_v1_flags(tmp_path):
    adapter = importlib.import_module("skills.orchestrate.scripts.adapters.opencode")
    plan = {
        "worktree": "",
        "cwd": str(tmp_path),
        "resume_session": "",
        "model": "opencode/mimo-v2.6-flash-free",
        "effort": "",
        "prompt": "hello",
    }
    argv = adapter.argv(plan)
    assert argv[:3] == ["opencode", "run", "--standalone"]
    assert "--dir" not in argv and "--variant" not in argv
    assert argv[argv.index("--model") + 1] == "opencode/mimo-v2.6-flash-free"


def test_opencode_v2_argv_carries_effort_as_model_variant_and_resume(tmp_path):
    adapter = importlib.import_module("skills.orchestrate.scripts.adapters.opencode")
    base = {"worktree": "", "cwd": str(tmp_path), "resume_session": "ses_1", "prompt": "hi"}
    argv = adapter.argv({**base, "model": "opencode/glm-5.3-flash-free", "effort": "high"})
    assert argv[argv.index("--model") + 1] == "opencode/glm-5.3-flash-free#high"
    assert argv[argv.index("-s") + 1] == "ses_1"
    # An id that already names its variant is not suffixed twice.
    argv = adapter.argv({**base, "model": "opencode/x#low", "effort": "high"})
    assert argv[argv.index("--model") + 1] == "opencode/x#low"


@pytest.mark.parametrize("text,ok", [("opencode v2.0.30", True), ("2.1.0", True), ("1.14.2", False),
                                     ("opencode v1.0.0", False), ("", True), ("garbage", True)])
def test_opencode_version_gate_names_a_fix_for_v1(text, ok):
    adapter = importlib.import_module("skills.orchestrate.scripts.adapters.opencode")
    error = adapter.version_error(text)
    assert (error is None) is ok
    assert ok or "fix: upgrade OpenCode to v2" in error


def test_opencode_version_probe_failure_is_not_cached(tmp_path, monkeypatch):
    cli = tmp_path / "bin" / "opencode"
    cli.parent.mkdir()
    cli.write_text("#!/bin/sh\necho nothing\n")
    cli.chmod(0o755)
    mod = supervisor()
    mod.provider_confinement._OPENCODE_VERSION_CACHE.clear()
    assert mod.provider_confinement._opencode_version_error("opencode", str(cli.parent)) is None
    assert mod.provider_confinement._OPENCODE_VERSION_CACHE == {}
    cli.write_text("#!/bin/sh\necho 1.2.3\n")
    assert "unsupported" in mod.provider_confinement._opencode_version_error("opencode", str(cli.parent))
    mod.provider_confinement._OPENCODE_VERSION_CACHE.clear()


def test_opencode_v1_cli_is_refused_at_planning(tmp_path, monkeypatch):
    cli = tmp_path / "bin" / "opencode"
    cli.parent.mkdir()
    cli.write_text("#!/bin/sh\necho 1.14.2\n")
    cli.chmod(0o755)
    monkeypatch.setenv("PATH", str(cli.parent) + ":" + os.environ["PATH"])
    mod = supervisor()
    mod.provider_confinement._OPENCODE_VERSION_CACHE.clear()
    with pytest.raises(ValueError, match="unsupported; fix: upgrade OpenCode to v2"):
        mod.build_plan("opencode", {"resolved_model": "opencode/x"}, "hello", cwd=tmp_path, workspace_root=tmp_path)
    mod.provider_confinement._OPENCODE_VERSION_CACHE.clear()


def test_opencode_bad_variant_fix_names_the_variant_to_drop():
    event = json.dumps({"type": "error", "error": {"type": "provider.no-route", "message": "Variant unavailable for opencode/x: high"}})
    fix = provider_output()._variant_fix(event)
    assert fix.startswith("opencode/x has no `high` variant; drop --effort or `#high`")
    assert provider_output()._variant_fix("plain input error") == "plain input error"


def test_opencode_v2_events_yield_text_session_and_terminal():
    lines = [
        {"type": "step_start", "sessionID": "ses_9", "part": {"type": "step-start"}},
        {"type": "tool_use", "sessionID": "ses_9", "part": {"tool": "read", "state": {"status": "completed"}}},
        {"type": "text", "sessionID": "ses_9", "part": {"type": "text", "text": "PONG"}},
        {"type": "step_finish", "sessionID": "ses_9", "part": {"type": "step-finish", "reason": "stop"}},
    ]
    parsed = provider_output().parse_output("opencode", "\n".join(map(json.dumps, lines)) + "\n", "", 0)
    assert (parsed["status"], parsed["text"], parsed["session_id"], parsed["terminal"]) == ("ok", "PONG", "ses_9", True)


def test_opencode_v2_single_text_event_without_step_events_is_ok():
    event = {"type": "text", "sessionID": "ses_9", "part": {"type": "text", "text": "PONG"}}
    assert provider_output().parse_output("opencode", json.dumps(event) + "\n", "", 0)["status"] == "ok"


@pytest.mark.parametrize("stdout", ["", json.dumps({"type": "step_start", "sessionID": "s", "part": {}}) + "\n",
                                    json.dumps({"type": "step_finish", "part": None}) + "\n"])
def test_opencode_exit_zero_without_assistant_text_is_empty_output(stdout):
    parsed = provider_output().parse_output("opencode", stdout, "", 0)
    assert (parsed["status"], parsed["signature"]) == ("failed", "empty_output")


def test_unsupported_controls_are_not_claimed_as_applied(tmp_path):
    plan = supervisor().build_plan(
        "cursor",
        {"resolved_model": "fixture", "effort": "high"},
        "hello",
        cwd=tmp_path,
        workspace_root=tmp_path,
        requested_effort="high",
        network=False,
    )
    assert plan["effort"] == ""
    assert plan["applied"]["network"] is None
    assert plan["warnings"]
    plan = supervisor().build_plan(
        "codex",
        {"resolved_model": "fixture"},
        "hello",
        cwd=tmp_path,
        workspace_root=tmp_path,
        mode="worktree_write",
        sandbox="full",
        network=False,
    )
    assert plan["applied"]["network"] is None
    assert plan["applied"]["guarantee"] != "enforced"


def test_kiro_acp_stream_captures_text_session_and_model():
    events = [
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "kiro-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": "DONE"},
                },
            },
        },
        {
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "kiro-1",
                "update": {
                    "sessionUpdate": "config_options_update",
                    "configOptions": [
                        {"category": "model", "currentValue": "kiro-auto"}
                    ],
                },
            },
        },
        {"jsonrpc": "2.0", "id": 1, "result": {"stopReason": "end_turn"}},
    ]
    parsed = provider_output().parse_output("kiro", "\n".join(map(json.dumps, events)))
    assert parsed["text"] == "DONE"
    assert parsed["session_id"] == "kiro-1"
    assert parsed["observed_model"] == "kiro-auto"
    assert parsed["terminal"] is True


def test_kiro_enforcement_requires_fresh_version_bound_negative_probe(tmp_path):
    from datetime import UTC, datetime, timedelta

    route = {
        "resolved_model": "auto",
        "cli_version": "2.23.0",
        "read_only_probe": {
            "cli_version": "2.23.0",
            "checked_at": datetime.now(UTC).isoformat(),
            "attempted_write": True,
            "permission_denied": True,
            "file_created": False,
        },
    }
    assert (
        supervisor().build_plan("kiro", route, "hello", cwd=tmp_path, workspace_root=tmp_path)["applied"][
            "guarantee"
        ]
        == "enforced"
    )
    route["read_only_probe"]["cli_version"] = "old"
    assert (
        supervisor().build_plan("kiro", route, "hello", cwd=tmp_path, workspace_root=tmp_path)["applied"][
            "guarantee"
        ]
        == "prompt_only"
    )
    route["read_only_probe"].update(
        cli_version="2.23.0",
        checked_at=(datetime.now(UTC) - timedelta(days=2)).isoformat(),
    )
    assert (
        supervisor().build_plan("kiro", route, "hello", cwd=tmp_path, workspace_root=tmp_path)["applied"][
            "guarantee"
        ]
        == "prompt_only"
    )


def test_writer_watchdog_observation_does_not_reset_silence(tmp_path):
    plan = fixture_plan(
        tmp_path,
        "import time; time.sleep(30)",
        mode="worktree_write",
        timeout_seconds=3.5,
    )
    plan["silence_seconds"] = 1
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "timed_out"
    observation = json.loads((tmp_path / "watchdog.json").read_text())
    assert observation["process_alive"] is True
    assert observation["silent_seconds"] >= 2

@pytest.mark.parametrize('api_key', [False, True])
def test_claude_plan_isolates_settings_and_mcp(tmp_path, monkeypatch, api_key):
    monkeypatch.delenv('ANTHROPIC_BASE_URL', raising=False)
    monkeypatch.delenv('ANTHROPIC_AUTH_TOKEN', raising=False)
    monkeypatch.delenv('ANTHROPIC_API_KEY', raising=False)
    if api_key:
        monkeypatch.setenv('ANTHROPIC_API_KEY', 'fixture-key')
    plan = supervisor().build_plan('claude', {'resolved_model': 'opus'}, 'hello', cwd=tmp_path,
                                   workspace_root=tmp_path)
    assert ('--bare' if api_key else '--safe-mode') in plan['argv']
    assert '--strict-mcp-config' in plan['argv']
    assert plan['argv'][plan['argv'].index('--permission-mode') + 1] == 'default'
    assert plan['argv'][plan['argv'].index('--permission-prompts') + 1] == 'none'
    assert plan['argv'][plan['argv'].index('--tools') + 1] == 'Read,Grep,Glob,Bash'
    assert plan['argv'][plan['argv'].index('--allowedTools') + 1] == 'Bash'
    boundary = plan['argv'][plan['argv'].index('--system-prompt') + 1]
    assert 'Do not modify files or run commands that mutate state.' in boundary
    assert 'Shell commands that do not modify files are allowed.' in boundary
    assert 'Use file-reading tools only.' not in boundary
    assert 'fixture-key' not in ' '.join(plan['argv'])


def test_question_example_in_middle_of_answer_does_not_suspend():
    parsed = provider_output().parse_output('claude', json.dumps({'type':'result','result':'Example:\n```\nQUESTION: Which?\n```\nEnd of explanation.'}))
    assert parsed['status'] == 'ok'


@pytest.mark.parametrize('directory', ['.config', 'Library', '.local/share', '.local'])
def test_add_dirs_deny_ancestors_of_credential_stores(tmp_path, monkeypatch, directory):
    monkeypatch.setenv('HOME', str(tmp_path))
    target = tmp_path / directory
    target.mkdir(parents=True)
    plan = supervisor().build_plan('codex', {}, 'hello', cwd=tmp_path, workspace_root=tmp_path, add_dirs=[target])
    assert str(target) not in plan['applied']['add_dirs']
    assert any('credential' in warning for warning in plan['warnings'])


@pytest.mark.parametrize('adapter', ['agy', 'claude', 'cursor', 'opencode', 'kiro'])
def test_wrapped_writer_records_add_dirs_in_write_boundary(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    add_dir = tmp_path / 'extra'
    add_dir.mkdir()
    force_sandbox(monkeypatch, '/usr/bin/sandbox-exec')
    plan = mod.build_plan(adapter, {}, 'hello', cwd=tmp_path, workspace_root=tmp_path,
                          mode='worktree_write', add_dirs=[add_dir])
    assert str(add_dir) in plan['applied']['add_dirs']
    assert str(add_dir) in plan['applied']['write_boundary']['writable_paths']


def test_opencode_does_not_claim_unsupported_add_dirs_without_wrapper(monkeypatch, tmp_path):
    mod = supervisor()
    force_sandbox(monkeypatch, None)
    plan = mod.build_plan('opencode', {}, 'hello', cwd=tmp_path, workspace_root=tmp_path, add_dirs=[tmp_path])
    assert plan['applied']['add_dirs'] == []
    assert 'additional directories unsupported by opencode' in plan['warnings']


@pytest.mark.parametrize('directory', [
    '.gemini', '.cursor', '.kiro', '.docker', '.kube', '.config/opencode',
    '.config/gh', '.config/gcloud', '.aws', '.ssh', '.gnupg', '.netrc',
    '.npmrc', '.local/share/opencode/auth.json',
])
def test_add_dirs_warns_and_drops_credential_stores(tmp_path, monkeypatch, directory):
    monkeypatch.setattr(Path, 'home', lambda: tmp_path)
    target = tmp_path / directory
    target.mkdir(parents=True)
    safe = tmp_path / 'source'
    safe.mkdir()
    plan = supervisor().build_plan('codex', {}, 'hello', cwd=tmp_path, workspace_root=tmp_path, add_dirs=[target, safe])
    assert str(target) not in plan['applied']['add_dirs']
    assert str(safe) in plan['applied']['add_dirs']
    assert any('credential' in warning for warning in plan['warnings'])


def test_kiro_tool_echo_does_not_override_observed_model():
    output = '\n'.join(map(json.dumps, [
        {'type':'system', 'model':'actual'},
        {'type':'tool_result','data':{'model':'echoed'}},
        {'type':'result','result':'DONE'},
    ]))
    assert provider_output().parse_output('kiro', output)['observed_model'] == 'actual'


def test_capture_rollover_writes_linear_bytes_and_preserves_terminal_result(tmp_path):
    mod = supervisor()
    capture = mod.BoundedCapture(tmp_path / 'capture', limit=1000)
    class Meter:
        def __init__(self, file):
            self.file, self.written = file, 0
        def write(self, data):
            self.written += len(data)
            return self.file.write(data)
        def __getattr__(self, name):
            return getattr(self.file, name)
    meter = Meter(capture.file)
    capture.file = meter
    for _ in range(1000):
        capture.write(b'x' * 100)
    capture.close()
    assert meter.written <= 102000
    assert (tmp_path / 'capture').stat().st_size == 1000


def test_stream_capture_retains_early_semantic_events_with_bounded_storage(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod.provider_observer, 'MAX_EVENTS_BYTES', 2048)
    code = '''import json
print(json.dumps({'type':'system','model':'observed','session_id':'retained'}))
print(json.dumps({'type':'result','result':'DONE'}))
for i in range(10000): print(json.dumps({'type':'telemetry','data':'x'*100}))
'''
    plan = fixture_plan(tmp_path, code, 'claude')
    record = mod.execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'ok'
    assert record['session_id'] == 'retained'
    assert (tmp_path / 'result.md').read_text() == 'DONE'
    assert (tmp_path / 'events.jsonl').stat().st_size <= 2048


def test_plan_uses_router_applied_effort(tmp_path):
    plan = supervisor().build_plan('opencode', {'resolved_model':'fixture', 'effort':'high', 'effort_applied':'medium'}, 'hello', cwd=tmp_path, workspace_root=tmp_path)
    assert plan['effort'] == 'medium'
    assert plan['argv'][plan['argv'].index('--model') + 1] == 'fixture#medium'
    assert '--variant' not in plan['argv']


def test_fallback_uses_router_candidates_and_parses_explicit_routes():
    mod = importlib.import_module('skills.orchestrate.scripts.exec_routing')
    plan = {'adapter':'claude','model':'opus','effort':'high','route':{'fallback_candidates':[{'adapter':'codex','model':'gpt-6.1-sol','effort_applied':'medium'}]}}
    assert mod.candidates(plan, True, {}) == [{'adapter':'codex','model':'gpt-6.1-sol','effort':'medium'}]
    assert mod.candidates(plan, ['codex/gpt-6.1-sol@low'], {}) == [{'adapter':'codex','model':'gpt-6.1-sol','effort':'low'}]


@pytest.mark.parametrize('policy', ['yes', 'codex/gpt-6.1-sol', 3, {}, [''], [3], [{'adapter': [], 'model': 'sol'}]])
def test_invalid_fallback_rejected_during_preflight(tmp_path, monkeypatch, policy):
    mod = importlib.import_module('skills.orchestrate.scripts.dispatch_preflight')
    monkeypatch.chdir(tmp_path)
    result = mod.preflight_tasks([{'id':'one','adapter':'claude','model':'opus','prompt':'hello','fallback':policy}])
    assert result['status'] == 'rejected'
    assert result['error'] == 'fallback_invalid'


def test_cancel_allows_provider_session_flush(tmp_path):
    code = '''import signal,time
from pathlib import Path
def stop(*args):
    time.sleep(.3)
    Path('flushed').write_text('saved')
    raise SystemExit(0)
signal.signal(signal.SIGTERM, stop)
Path('ready').touch()
time.sleep(30)
'''
    plan = fixture_plan(tmp_path, code, timeout_seconds=5)
    record = supervisor().execute(plan, tmp_path / 'result.md', cancelled=lambda: (tmp_path / 'ready').exists())
    assert record['status'] == 'cancelled'
    assert (tmp_path / 'flushed').read_text() == 'saved'


def test_writer_progress_probe_is_bounded_and_reaches_late_files(tmp_path):
    mod = supervisor()
    directory = tmp_path / 'source'
    directory.mkdir()
    for number in range(60000):
        (directory / str(number)).touch()
    scanner = mod.WorkspaceProgress(tmp_path)
    while not scanner.initial_pass_complete:
        scanner.probe()
        assert scanner.last_visited <= 2000
    last = list(os.walk(directory))[0][2][-1]
    (directory / last).write_text('changed')
    # A slow host may need many probes; one full later pass is the real bound.
    for _ in range(60001):
        changed = scanner.probe()
        assert scanner.last_visited <= 2000
        if changed:
            break
    assert changed


def test_writer_progress_uses_string_keys_for_large_tree_memory(tmp_path):
    mod = supervisor()
    (tmp_path / 'source.txt').touch()
    scanner = mod.WorkspaceProgress(tmp_path)
    scanner.probe()
    assert scanner.known and all(type(key) is str for key in scanner.known)
    assert scanner._seen and all(type(key) is str for key in scanner._seen)


def test_writer_progress_advances_when_probe_budget_expires_immediately(tmp_path, monkeypatch):
    mod = supervisor()
    for number in range(10):
        (tmp_path / str(number)).touch()
    ticks = iter(number * 0.1 for number in range(10000))
    monkeypatch.setattr(mod, 'time', SimpleNamespace(time_ns=time.time_ns,
                                                     monotonic=lambda: next(ticks)))
    scanner = mod.WorkspaceProgress(tmp_path)
    for _ in range(10):
        scanner.probe()
        if scanner.initial_pass_complete:
            break
    assert scanner.initial_pass_complete


def test_writer_progress_tolerates_coarse_creation_clock(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod, 'time', SimpleNamespace(time_ns=lambda: time.time_ns() + 10_000_000,
                                                     monotonic=time.monotonic))
    scanner = mod.WorkspaceProgress(tmp_path)
    copied = tmp_path / 'copied.txt'
    copied.write_text('new file')
    os.utime(copied, (1, 1))
    assert scanner.probe()


def test_writer_wall_deadline_does_not_wait_for_silence_interval(tmp_path):
    plan = fixture_plan(tmp_path, 'import time; time.sleep(30)',
                        mode='worktree_write', timeout_seconds=1.6)
    record = supervisor().execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'timed_out'


def test_writer_progress_sees_preserved_mtime_copy_during_initial_scan(tmp_path):
    mod = supervisor()
    directory = tmp_path / 'source'
    directory.mkdir()
    scanner = mod.WorkspaceProgress(tmp_path)
    copied = directory / 'copied.txt'
    copied.write_text('new file')
    os.utime(copied, (1, 1))
    assert scanner.probe()


def test_writer_progress_sees_preserved_mtime_copy_at_root(tmp_path):
    mod = supervisor()
    scanner = mod.WorkspaceProgress(tmp_path)
    copied = tmp_path / 'copied.txt'
    copied.write_text('new file')
    os.utime(copied, (1, 1))
    assert scanner.probe()


def test_writer_progress_tracks_deletion_without_counting_excluded_paths(tmp_path):
    mod = supervisor()
    directory = tmp_path / 'source'
    directory.mkdir()
    source = directory / 'source.txt'
    source.write_text('content')
    ignored = tmp_path / '.git'
    ignored.mkdir()
    metadata = ignored / 'metadata'
    metadata.write_text('before')
    output = tmp_path / 'events.jsonl'
    output.write_text('before')
    scanner = mod.WorkspaceProgress(tmp_path, {output})
    assert not scanner.probe()
    output.write_text('after')
    metadata.write_text('after')
    assert not scanner.probe()
    source.unlink()
    assert scanner.probe()
    (directory / 'added.txt').write_text('new')
    assert scanner.probe()


def test_writer_progress_ignores_new_excluded_and_ignored_paths(tmp_path):
    mod = supervisor()
    output = tmp_path / 'events.jsonl'
    scanner = mod.WorkspaceProgress(tmp_path, {output})
    output.write_text('event')
    (tmp_path / 'node_modules').mkdir()
    assert not scanner.probe()


def test_stream_rollover_preserves_unclassified_structured_failure(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod.provider_observer, 'MAX_EVENTS_BYTES', 2048)
    code = '''import json
print(json.dumps({'type':'result','is_error':True,'result':'backend exploded'}))
for i in range(1000): print(json.dumps({'type':'telemetry','data':'x'*100}))
'''
    record = mod.execute(fixture_plan(tmp_path, code, 'claude'), tmp_path / 'result.md')
    assert record['status'] == 'failed'


def test_explicit_route_fallback_policy_reaches_router(tmp_path, monkeypatch):
    mod = importlib.import_module('skills.orchestrate.scripts.dispatch_preflight')
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(ROOT))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(ROOT))
    result = mod.preflight_tasks([{'id':'one','adapter':'claude','model':'opus','prompt':'hello','fallback':True}])
    assert result['status'] == 'validated'
    assert result['routes'][0]['fallback_candidates']


def test_oversized_plain_result_is_partial_and_not_certifiable(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod.provider_observer, 'MAX_EVENTS_BYTES', 2048)
    plan = fixture_plan(tmp_path, "print('a' * 10000)", intent='assurance', orchestrator_family='anthropic')
    record = mod.execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'partial'
    assert not record['certification_eligible']
    assert any('truncated' in warning for warning in record['warnings'])
    assert (tmp_path / 'result.md').stat().st_size <= 2048


def test_oversized_split_result_cannot_certify_preliminary_text(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod.provider_observer, 'MAX_EVENTS_BYTES', 2048)
    code = '''import json,sys,time
print(json.dumps({'type':'assistant','message':{'content':'preliminary'}}), flush=True)
result = json.dumps({'type':'result','result':'a'*100000})+'\\n'
for offset in range(0, len(result), 1024):
    sys.stdout.write(result[offset:offset+1024]); sys.stdout.flush(); time.sleep(.001)
'''
    record = mod.execute(fixture_plan(tmp_path, code, 'claude'), tmp_path / 'result.md')
    assert record['status'] == 'partial'
    assert any('truncated' in warning for warning in record['warnings'])


def test_malformed_router_fallback_does_not_crash_finished_attempt():
    mod = importlib.import_module('skills.orchestrate.scripts.exec_routing')
    plan = {'adapter':'claude','model':'opus','route':{'fallback_candidates':[None, {'adapter':'future','model':'x'}, {'adapter':'codex','model':'sol'}]}}
    assert mod.candidates(plan, True, {}) == [{'adapter':'codex','model':'sol','effort':None}]


@pytest.mark.parametrize('rollover', [False, True])
def test_successful_terminal_result_supersedes_transient_retry_error(tmp_path, monkeypatch, rollover):
    mod = supervisor()
    monkeypatch.setattr(mod.provider_observer, 'MAX_EVENTS_BYTES', 2048 if rollover else 20*1024*1024)
    code = '''import json
print(json.dumps({'type':'api_retry','error':'529 overloaded'}))
for i in range(1000): print(json.dumps({'type':'telemetry','data':'x'*100}))
print(json.dumps({'type':'result','is_error':False,'result':'DONE'}))
'''
    record = mod.execute(fixture_plan(tmp_path, code, 'claude'), tmp_path / 'result.md')
    assert record['status'] == 'ok'


@pytest.mark.parametrize('rollover', [False, True])
def test_retry_success_never_erases_a_separate_hard_failure(tmp_path, monkeypatch, rollover):
    mod = supervisor()
    monkeypatch.setattr(mod.provider_observer, 'MAX_EVENTS_BYTES', 2048 if rollover else 20*1024*1024)
    code = '''import json
print(json.dumps({'type':'error','error':'permission denied'}))
print(json.dumps({'type':'api_retry','error':'529 overloaded'}))
for i in range(1000): print(json.dumps({'type':'telemetry','data':'x'*100}))
print(json.dumps({'type':'result','is_error':False,'result':'DONE'}))
'''
    record = mod.execute(fixture_plan(tmp_path, code, 'claude'), tmp_path / 'result.md')
    assert record['status'] == 'permission_blocked'


@pytest.mark.parametrize('raw,models,expected', [(None, {}, []), (3, {}, []), ([{'adapter':'codex','model':'sol'}], None, ['sol']), ([{'adapter':'codex','model':'sol'}], {'sol':None}, ['sol'])])
def test_malformed_router_candidate_containers_are_tolerated(raw, models, expected):
    mod = importlib.import_module('skills.orchestrate.scripts.exec_routing')
    plan = {'adapter':'claude','model':'opus','route':{'fallback_candidates':raw}}
    assert [item['model'] for item in mod.candidates(plan, True, {'models':models})] == expected


def test_agy_nested_result_envelope_is_success_with_observed_model():
    # Shape captured from a live agy run (2026-09-23): kind under `event`,
    # model in `init`, status/response nested under `result`.
    events = [
        {"event": "init", "conversation_id": "c1", "init": {"model": "gemini-3.8-flash-high"}},
        {"event": "step_update", "step_update": {"step_index": 1, "state": "DONE",
         "step_type": "agent_response", "text_delta": "PONG"}},
        {"event": "result", "result": {"conversation_id": "c1", "status": "SUCCESS",
         "response": "PONG\n", "num_turns": 1}},
    ]
    parsed = provider_output().parse_output("agy", "\n".join(map(json.dumps, events)))
    assert parsed["status"] == "ok"
    assert parsed["text"] == "PONG\n"
    assert parsed["observed_model"] == "gemini-3.8-flash-high"


def test_agy_nested_result_failure_keeps_provider_error():
    events = [
        {"event": "result", "result": {"status": "FAILED", "response": "",
         "error": "RESOURCE_EXHAUSTED (code 429): Individual quota reached. Resets in 29m44s."}},
    ]
    parsed = provider_output().parse_output("agy", "\n".join(map(json.dumps, events)), exit_code=3)
    assert parsed["status"] != "ok"
    assert "quota" in parsed["excerpt"].lower()


def test_kiro_stream_json_selects_the_v3_engine():
    from adapters import kiro
    command = kiro.argv({"mode": "read_only", "resume_session": None, "model": "auto",
                         "effort": None, "boundary_prompt": "B", "prompt": "P"})
    # kiro-cli 2.23's v2 engine never finishes a headless turn; v3 emits the same stream.
    assert command[command.index("--agent-engine") + 1] == "v3"
    assert command.index("--agent-engine") < command.index("--output-format")


@pytest.mark.parametrize(
    "adapter,resolved,observed,same",
    [
        ("claude", "opus", "claude-opus-5-5", True),
        ("cursor", "grok-4.7", "Grok 4.7 256K High Fast", True),
        ("cursor", "auto", "Auto", True),
        ("codex", "gpt-6-luna", "gpt-6.1-sol", False),
        ("agy", "gemini-3.8-flash", "claude-opus-4-6", False),
        ("agy", "claude-opus-4-6", "claude-opus-4-6-thinking", False),
    ],
)
def test_alias_and_display_names_are_not_substitutions(adapter, resolved, observed, same):
    assert provider_output()._same_model(adapter, resolved, observed) is same


def test_kiro_v2_engine_stream_is_parsed_and_auto_is_not_passed():
    # Shape captured from a live kiro-cli --agent-engine v2 run (2026-09-23).
    events = [
        {"type": "runStarted", "data": {"payloadSchema": "acp", "engine": "v2"}},
        {"type": "sessionUpdate", "data": {"sessionId": "k2", "update": {
            "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "PONG"}}}},
        {"type": "runFinished", "data": {"sessionId": "k2", "status": "success",
         "stopReason": "end_turn", "finalText": "PONG"}},
    ]
    parsed = provider_output().parse_output("kiro", "\n".join(map(json.dumps, events)))
    assert (parsed["status"], parsed["text"], parsed["session_id"]) == ("ok", "PONG", "k2")
    from adapters import kiro
    command = kiro.argv({"mode": "read_only", "resume_session": None, "model": "auto",
                         "effort": None, "boundary_prompt": "B", "prompt": "P"})
    assert "--model" not in command


def test_claude_reported_ids_match_their_aliases():
    module = provider_output()
    assert module._same_model("claude", "haiku", "claude-haiku-5-5")
    assert not module._same_model("claude", "haiku", "claude-haiku-4-5-20251001")
    assert module._same_model("claude", "claude-haiku-4-5", "claude-haiku-4-5-20251001")
    assert not module._same_model("claude", "claude-haiku-4-5", "claude-haiku-5-5")
    assert module._same_model("claude", "sonnet", "claude-sonnet-5-5")
    assert module._same_model("claude", "claude-sonnet-5-5", "claude-sonnet-5")
    assert not module._same_model("claude", "haiku", "claude-sonnet-5-5")


def test_subreaper_is_held_only_while_attempts_run(tmp_path, monkeypatch):
    # A process left as a child subreaper adopts unrelated orphans it never
    # reaps; a detached server then lingers as a zombie of the caller.
    module = supervisor()
    calls = []

    def prctl(option, argument):
        calls.append((option, argument if isinstance(argument, int) else "get"))
        return 0

    monkeypatch.setattr(module.sys, "platform", "linux")
    monkeypatch.setattr(module, "_prctl", prctl)
    monkeypatch.setattr(module, "_SUBREAPER", {"users": 0, "previous": 0})
    assert module._enable_subreaper() and module._enable_subreaper()
    module._release_subreaper()
    assert (36, 0) not in calls  # another attempt still runs
    module._release_subreaper()
    assert calls == [(37, "get"), (36, 1), (36, 0)]


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="PR_SET_CHILD_SUBREAPER is Linux-only")
def test_attempt_restores_the_callers_subreaper_setting(tmp_path):
    module = supervisor()
    plan = fixture_plan(tmp_path, "import json; print(json.dumps({'type':'result','result':'DONE','is_error':False}))")
    assert module.execute(plan, tmp_path / "result.md")["status"] == "ok"
    value = ctypes.c_int(-1)
    ctypes.CDLL(None, use_errno=True).prctl(37, ctypes.byref(value), 0, 0, 0)
    assert value.value == 0


def test_subreaper_is_released_when_attempt_cleanup_raises(tmp_path, monkeypatch):
    module = supervisor()
    held = []
    monkeypatch.setattr(module, "_enable_subreaper", lambda: held.append("on") or True)
    monkeypatch.setattr(module, "_release_subreaper", lambda: held.append("off"))

    import selectors

    class FailingClose(selectors.DefaultSelector):
        def close(self):
            super().close()
            raise RuntimeError("cleanup failed")

    monkeypatch.setattr(selectors, "DefaultSelector", FailingClose)
    plan = fixture_plan(tmp_path, "import json; print(json.dumps({'type':'result','result':'DONE','is_error':False}))")
    with pytest.raises(RuntimeError, match="cleanup failed"):
        module.execute(plan, tmp_path / "result.md")
    assert held == ["on", "off"]


# Sandbox false reds in lanes (#897).

def test_attempt_points_corepack_at_the_attempt_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("COREPACK_HOME", str(tmp_path / "outside-the-writable-roots"))
    code = """import json, os
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':json.dumps({key: os.environ.get(key) for key in ('COREPACK_HOME','XDG_CACHE_HOME')})}}))
print(json.dumps({'type':'turn.completed'}))
"""
    record = supervisor().execute(fixture_plan(tmp_path, code), tmp_path / "result.md")
    assert record["status"] == "ok", record
    assert json.loads((tmp_path / "result.md").read_text()) == {
        "COREPACK_HOME": str(tmp_path / "tmp/cache/node/corepack"),
        "XDG_CACHE_HOME": str(tmp_path / "tmp/cache"),
    }


def test_codex_writer_keeps_the_attempt_temp_a_writable_root(tmp_path):
    _, lane = instruction_lane(tmp_path)
    for resume in (None, "saved"):
        plan = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello", mode="worktree_write",
                                       worktree=lane, workspace_root=tmp_path, resume_session=resume)
        assert codex_filesystem(plan)[Path(":tmpdir")] == "write"
        assert plan["applied"]["write_boundary"]["filesystem"][":tmpdir"] == "write"


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
@pytest.mark.parametrize("adapter", ["agy", "claude", "cursor", "kiro", "opencode", "codex"])
def test_confined_writer_merges_protected_paths_and_fills_its_caches(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    _, lane = instruction_lane(tmp_path)
    attempt = tmp_path / "attempt"
    attempt.mkdir()
    # Codex takes the sandbox-exec profile only in a capability lane.
    controls = {"network": True, "capabilities": ["postgres"]} if adapter == "codex" else {}
    plan = mod.build_plan(adapter, {"resolved_model": "fixture"}, "hello", mode="worktree_write",
                          worktree=lane, workspace_root=tmp_path, run_dir=str(attempt), **controls)
    assert plan["applied"]["confinement"] == "sandbox-exec"
    cache = attempt / "tmp/cache"
    script = (f"mkdir -p '{cache}/node/corepack' '{cache}/gitleaks' && "
              "git " + " ".join(GIT_FIXTURE) + " merge -q --no-edit main")
    result = subprocess.run([sandbox_exec, "-p", mod.provider_confinement.os_confinement_profile(plan), "/bin/sh", "-c", script],
                            cwd=lane, capture_output=True, text=True)
    if result.returncode and "sandbox_apply" in result.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert result.returncode == 0, result.stderr
    assert (lane / SKILL).read_text() == "v2\n"
    assert (cache / "node/corepack").is_dir()


def first_instruction_lane(tmp_path):
    """A lane cut before main adds the repository's first skill."""
    repo = tmp_path / "repo"
    git_repo(repo, branch="main", files={"app.txt": "app\n"}, commit=False)
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "initial")
    lane = repo / ".worktrees/lane"
    make_linked_worktree(repo, "lane", path=lane)
    (repo / SKILL).parent.mkdir(parents=True)
    (repo / SKILL).write_text("v1\n")
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "main adds a skill")
    return repo, lane


def test_codex_writer_is_granted_an_absent_instruction_directory(tmp_path):
    _, lane = first_instruction_lane(tmp_path)
    plan = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello",
                                   mode="worktree_write", worktree=lane, workspace_root=tmp_path)
    # Codex protects .agents in a writable root even before it exists.
    assert str(lane / ".agents") in plan["applied"]["add_dirs"]
    assert not (lane / ".agents").exists()


def test_codex_writer_may_merge_in_a_first_instruction_directory(tmp_path):
    _, lane = first_instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, "git('merge', '-q', '--no-edit', 'main')\n")
    assert record["status"] == "ok", record
    assert (lane / SKILL).read_text() == "v1\n"


def test_codex_writer_authoring_a_first_instruction_directory_is_quarantined(tmp_path):
    _, lane = first_instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, "open('.agents/own.md', 'w').write('lane\\n')\n")
    assert record["status"] == "quarantined", record
    assert any(".agents/own.md" in warning and "quarantined" in warning for warning in record["warnings"])
    assert not os.path.lexists(lane / ".agents")
    assert "+lane" in (tmp_path / "attempt" / "protected.patch").read_text()


def test_codex_writer_leaves_no_instruction_directory_it_did_not_need(tmp_path):
    _, lane = first_instruction_lane(tmp_path)
    observed = tmp_path / "seen"
    record = lane_attempt(tmp_path, lane, f"open({str(observed)!r}, 'w').write(str(os.path.isdir('.agents')))\n")
    assert record["status"] == "ok", record
    assert observed.read_text() == "True"
    assert not os.path.lexists(lane / ".agents")


@pytest.mark.parametrize("entry", ["file", "symlink"])
def test_codex_writer_is_not_granted_a_non_directory_instruction_entry(tmp_path, entry):
    _, lane = first_instruction_lane(tmp_path)
    if entry == "file":
        (lane / ".agents").write_text("not a directory\n")
    else:
        (tmp_path / "elsewhere").mkdir()
        (lane / ".agents").symlink_to(tmp_path / "elsewhere")
    plan = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello",
                                   mode="worktree_write", worktree=lane, workspace_root=tmp_path)
    assert str(lane / ".agents") not in plan["applied"]["add_dirs"]


def codex_read_only_permissions(argv):
    """The -c values that define Codex's read-only permission profile, with its per-plan name as NAME."""
    name, _ = codex_profile({"argv": argv})
    return [argv[index + 1].replace(name, "NAME") for index, arg in enumerate(argv[:-1])
            if arg == "-c" and argv[index + 1].startswith(("default_permissions=", "permissions."))]


def codex_sandbox_command(plan, repo):
    """A `codex sandbox` command applying the permission profile a Codex plan selects."""
    name, overrides = codex_profile(plan)
    command = ["codex", "sandbox", "-P", name, "-C", str(repo)]
    for value in ["default_permissions=" + json.dumps(name), *overrides]:
        command += ["-c", value]
    return command


def test_codex_read_only_writes_its_temp_and_add_dirs_but_not_the_cwd(tmp_path):
    repo = tmp_path / "repo"
    locks = repo / ".agent-run/locks"
    locks.mkdir(parents=True)
    plan = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello", cwd=repo,
                                   workspace_root=repo, add_dirs=[str(locks), str(repo)], network=False)
    assert codex_read_only_permissions(plan["argv"]) == [
        'default_permissions="NAME"',
        'permissions.NAME.extends=":read-only"',
        "permissions.NAME.network.enabled=false",
        'permissions.NAME.filesystem={":tmpdir" = "write", ' + json.dumps(str(locks)) + ' = "write", '
        + json.dumps(provenant_state()) + ' = "write"}',
    ]
    assert "-s" not in plan["argv"]
    assert f"codex read_only add-dir stays read-only (it holds cwd or a pattern character): {repo}" in plan["warnings"]
    networked = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello", cwd=repo,
                                        workspace_root=repo, network=True, resume_session="saved")
    assert "permissions.NAME.network.enabled=true" in codex_read_only_permissions(networked["argv"])


@pytest.mark.live_provider
@pytest.mark.skipif(sys.platform != "darwin" or not shutil.which("codex"), reason="needs the Codex seatbelt")
def test_codex_read_only_profile_writes_only_temp_and_lock_dirs(tmp_path):
    repo = tmp_path / "repo"
    locks = repo / ".agent-run/locks"
    locks.mkdir(parents=True)
    temp = tmp_path / "attempt/tmp"
    temp.mkdir(parents=True)
    (tmp_path / "codex-home").mkdir()
    plan = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello", cwd=repo,
                                   workspace_root=repo, add_dirs=[str(locks)], network=False)
    command = codex_sandbox_command(plan, repo)
    probe = subprocess.run(
        [*command, "--", "/bin/sh", "-c",
         f"touch '{locks}/held' && touch '{temp}/scratch' && ! touch '{repo}/review.txt' 2>/dev/null"],
        capture_output=True, text=True, timeout=60,
        env={**os.environ, "TMPDIR": str(temp), "CODEX_HOME": str(tmp_path / "codex-home")},
    )
    if probe.returncode and "sandbox" in probe.stderr.lower() and "not permitted" not in probe.stderr:
        pytest.skip("the Codex seatbelt cannot start here: " + probe.stderr[-200:])
    assert probe.returncode == 0, probe.stderr
    assert (locks / "held").exists() and (temp / "scratch").exists()
    assert not (repo / "review.txt").exists()


def opencode_config_repo(tmp_path):
    repo = tmp_path / "repo"
    cwd = repo / "packages/app"
    cwd.mkdir(parents=True)
    git(tmp_path, "init", "-q", str(repo))
    (repo / "opencode.json").write_text('{"permission": {"read": {"secrets/**": "deny"}}}\n')
    (repo / "AGENTS.md").write_text("agents\n")
    (repo / ".opencode/agent").mkdir(parents=True)
    (repo / ".opencode/agent/review.md").write_text("agent\n")
    (repo / ".opencode/.gitignore").write_text("node_modules\n")
    (repo / "secret.txt").write_text("secret\n")
    (repo / "opencode.jsonc").symlink_to(repo / "secret.txt")
    plan = {"adapter": "opencode", "mode": "read_only", "workspace_root": str(repo), "cwd": str(cwd),
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    return repo, cwd, plan


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_opencode_read_only_profile_reads_project_config_above_its_cwd(tmp_path):
    mod = supervisor()
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    repo, _, plan = opencode_config_repo(tmp_path)
    profile = mod.provider_confinement.os_confinement_profile(plan)

    def cat(path):
        return subprocess.run([sandbox_exec, "-p", profile, "/bin/cat", str(path)], capture_output=True, text=True)

    probe = cat(repo / "opencode.json")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert probe.returncode == 0, probe.stderr
    assert cat(repo / "AGENTS.md").returncode == 0
    assert cat(repo / ".opencode/agent/review.md").returncode == 0
    assert cat(repo / "secret.txt").returncode != 0
    # A link under a config name cannot carry the grant to another file.
    assert cat(repo / "opencode.jsonc").returncode != 0


@pytest.mark.parametrize("mode", ["read_only", "worktree_write"])
def test_opencode_discovery_grants_only_ancestor_directory_metadata(tmp_path, mode):
    repo, cwd = sandbox_profile_worktree(tmp_path)
    plan = {"adapter": "opencode", "mode": mode, "workspace_root": str(cwd), "cwd": str(cwd),
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    plan["protected_paths"] = [str(repo / name) for name in (".claude", ".codex")]
    profile = supervisor().provider_confinement.os_confinement_profile(plan)
    deny_at = profile.rindex('(deny file-read*')
    for ancestor in (cwd, *cwd.parents):
        for name in (".claude", ".codex"):
            grant = f'(literal "{ancestor / name}")'
            assert any(line.startswith("(allow file-read-data file-read-metadata ") and grant in line
                       for line in profile[deny_at:].splitlines())
    assert not any(line.startswith("(allow file-read-data file-read-metadata ") and '(subpath ' in line
                   for line in profile[deny_at:].splitlines())


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
@pytest.mark.parametrize("mode", ["read_only", "worktree_write"])
@pytest.mark.parametrize("symlink", [False, True])
def test_opencode_discovery_lstats_denied_ancestor_without_reading_contents(tmp_path, monkeypatch, mode, symlink):
    mod = supervisor()
    # The profile denies reads under HOME; a uv/pyenv interpreter lives there, so point HOME elsewhere.
    monkeypatch.setattr(mod.Path, "home", lambda: tmp_path / "home")
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    repo, cwd = sandbox_profile_worktree(tmp_path)
    plan = {"adapter": "opencode", "mode": mode, "workspace_root": str(cwd), "cwd": str(cwd),
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    protected = [repo / name for name in (".claude", ".codex")]
    targets = [repo / ("private-" + path.name) for path in protected] if symlink else protected
    for directory, target in zip(protected, targets):
        target.mkdir()
        (target / "secret.txt").write_text("private")
        if symlink:
            directory.symlink_to(target, target_is_directory=True)
    plan["protected_paths"] = list(map(str, targets))
    # OpenCode's realPath opens the directory itself (a bare lstat grant still returns EPERM live, #1012):
    # a real directory must open, while a planted link must not carry the grant to its target.
    script = """import os, sys
opens = sys.argv[1] == 'open'
for path in sys.argv[2:]:
    os.lstat(path)
    if opens:
        os.close(os.open(path, os.O_RDONLY))
    for operation in (lambda: open(path + '/secret.txt').read(), lambda: os.stat(path + '/secret.txt')):
        try:
            operation()
        except PermissionError:
            continue
        raise AssertionError('protected content or descendant metadata became readable')
print('metadata only')
"""
    # The system interpreter stays readable under the profile; a uv or venv one lives under the denied HOME.
    interpreter = "/usr/bin/python3" if os.access("/usr/bin/python3", os.X_OK) else sys.executable
    result = subprocess.run([sandbox_exec, "-p", mod.provider_confinement.os_confinement_profile(plan), interpreter,
                             "-c", script, "link" if symlink else "open", *map(str, protected)], cwd=cwd,
                            capture_output=True, text=True, timeout=10)
    if result.returncode and "sandbox_apply" in result.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "metadata only"


@pytest.mark.live_provider
@pytest.mark.skipif(sys.platform != "darwin" or not shutil.which("opencode"), reason="needs OpenCode")
def test_opencode_starts_below_a_repository_config(tmp_path):
    mod = supervisor()
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    _, cwd, plan = opencode_config_repo(tmp_path)
    (cwd.parents[1] / "opencode.jsonc").unlink()
    attempt = tmp_path / "attempt"
    (attempt / "tmp/cache").mkdir(parents=True)
    plan["run_dir"] = str(attempt)
    result = subprocess.run(
        [sandbox_exec, "-p", mod.provider_confinement.os_confinement_profile(plan), "opencode", "debug", "config"],
        cwd=cwd, capture_output=True, text=True, timeout=180,
        env={**os.environ, "TMPDIR": str(attempt / "tmp"), "XDG_CACHE_HOME": str(attempt / "tmp/cache")},
    )
    assert "FileSystem.readFile" not in result.stdout + result.stderr
    assert result.returncode == 0, result.stdout + result.stderr
    assert '"secrets/**"' in result.stdout


def confined_read_only_attempt(monkeypatch, tmp_path, script):
    """Run a shell script as a Claude read-only lane under its real sandbox-exec profile.

    The lane reviews <repo>/runtime/fabric, so the rest of the repository, home (where the
    product checkout lives) and shared temp are unreadable to it.
    """
    mod = supervisor()
    if sys.platform != "darwin" or not mod.provider_confinement._sandbox_exec_path():
        pytest.skip("sandbox-exec is unavailable or disabled")
    if subprocess.run(["/usr/bin/python3", "-c", ""], capture_output=True).returncode:
        pytest.skip("the system python3 is unavailable")
    monkeypatch.delenv("PROVENANT_NO_OS_CONFINEMENT", raising=False)
    repo = tmp_path / "repo"
    cwd = repo / "runtime/fabric"
    cwd.mkdir(parents=True)
    git(tmp_path, "init", "-q", str(repo))
    attempt = tmp_path / "attempt"
    attempt.mkdir()
    plan = mod.build_plan("claude", {"resolved_model": "fixture"}, "hello", cwd=cwd,
                          workspace_root=repo, run_dir=str(attempt))
    assert plan["applied"]["confinement"] == "sandbox-exec"
    plan["argv"] = ["/bin/sh", "-c", script
                    + "\necho '{\"type\":\"result\",\"result\":\"done\",\"is_error\":false}'"]
    plan["grace_seconds"] = 0.1
    probe = subprocess.run([mod.provider_confinement._sandbox_exec_path(), "-p", mod.provider_confinement.os_confinement_profile(plan), "/usr/bin/true"],
                           capture_output=True, text=True)
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    # A fixed PATH keeps the lane independent of the host toolchain, covered separately below.
    record = mod.execute(plan, attempt / "result.md", env={**os.environ, "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"})
    return record, repo, cwd, attempt


@pytest.mark.skipif(sys.platform != "darwin", reason="macOS refuses setuid ps in any sandbox")
def test_confined_read_only_lane_runs_the_ps_shim(monkeypatch, tmp_path):
    out = tmp_path / "attempt/ps.txt"
    record, _, _, attempt = confined_read_only_attempt(
        monkeypatch, tmp_path, f"command -v ps > '{out}' && ps -o pid= -p $$ >> '{out}' 2>&1")
    assert record["status"] == "ok", record
    found, pid = out.read_text().splitlines()
    assert found == str(attempt / "tmp/provenant-shim/bin/ps")
    assert pid.strip().isdigit()


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_confined_read_only_lane_has_writable_tool_caches_but_not_the_workspace(monkeypatch, tmp_path):
    out = tmp_path / "attempt/caches.json"
    check = f"""
import json, os, pathlib, tempfile
result = {{"tempfile": bool(tempfile.mkstemp()[1])}}
for key in ("UV_CACHE_DIR", "npm_config_cache", "PYTHONPYCACHEPREFIX", "RUFF_CACHE_DIR", "MYPY_CACHE_DIR",
            "COREPACK_HOME", "XDG_CACHE_HOME"):
    path = pathlib.Path(os.environ[key]) / "probe"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("ok")
    result[key] = str(path.parent)
try:
    pathlib.Path(".pytest_cache").mkdir()
    result["workspace"] = "writable"
except OSError:
    result["workspace"] = "read-only"
result["PYTEST_ADDOPTS"] = os.environ["PYTEST_ADDOPTS"]
pathlib.Path({str(out)!r}).write_text(json.dumps(result))
"""
    record, _, cwd, attempt = confined_read_only_attempt(
        monkeypatch, tmp_path, "/usr/bin/python3 -c " + shlex.quote(check))
    assert record["status"] == "ok", record
    result = json.loads(out.read_text())
    cache = attempt / "tmp/cache"
    assert result == {
        "tempfile": True, "workspace": "read-only", "PYTEST_ADDOPTS": "-p no:cacheprovider",
        "UV_CACHE_DIR": str(cache / "uv"), "npm_config_cache": str(cache / "npm"),
        "PYTHONPYCACHEPREFIX": str(cache / "pycache"), "RUFF_CACHE_DIR": str(cache / "ruff"),
        "MYPY_CACHE_DIR": str(cache / "mypy"), "COREPACK_HOME": str(cache / "node/corepack"),
        "XDG_CACHE_HOME": str(cache),
    }
    assert not (cwd / ".pytest_cache").exists()


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_claude_read_only_shell_cannot_write_outside_the_attempt(monkeypatch, tmp_path):
    attempt = tmp_path / "attempt"
    cwd = tmp_path / "repo/runtime/fabric"
    targets = [cwd / "shell-write", tmp_path / "outside-attempt"]
    script = f"touch {shlex.quote(str(attempt / 'shell-write'))} || exit 1\n"
    for target in targets:
        script += f"if touch {shlex.quote(str(target))}; then exit 1; fi\n"
    script += "echo READ_ONLY_SHELL_OK"
    record, _, _, attempt = confined_read_only_attempt(monkeypatch, tmp_path, script)
    assert record["status"] == "ok", record
    assert (attempt / "shell-write").exists()
    assert all(not target.exists() for target in targets)


def test_tool_caches_override_inherited_locations(tmp_path, monkeypatch):
    monkeypatch.setenv("UV_CACHE_DIR", "/denied/uv")
    monkeypatch.setenv("npm_config_cache", "/denied/npm")
    monkeypatch.setenv("PYTEST_ADDOPTS", "-q")
    code = """import json, os
keys = ('UV_CACHE_DIR','npm_config_cache','PYTEST_ADDOPTS','PYTHONPYCACHEPREFIX')
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':json.dumps({key: os.environ.get(key) for key in keys})}}))
print(json.dumps({'type':'turn.completed'}))
"""
    record = supervisor().execute(fixture_plan(tmp_path, code), tmp_path / "result.md")
    assert record["status"] == "ok", record
    assert json.loads((tmp_path / "result.md").read_text()) == {
        "UV_CACHE_DIR": str(tmp_path / "tmp/cache/uv"), "npm_config_cache": str(tmp_path / "tmp/cache/npm"),
        "PYTEST_ADDOPTS": "-q -p no:cacheprovider", "PYTHONPYCACHEPREFIX": str(tmp_path / "tmp/cache/pycache"),
    }
    (tmp_path / "writer").mkdir()
    _, lane = instruction_lane(tmp_path / "writer")
    (tmp_path / "writer/attempt").mkdir()
    writer = fixture_plan(tmp_path / "writer/attempt", code, mode="worktree_write", worktree=lane)
    supervisor().execute(writer, tmp_path / "writer/attempt/result.md")
    # A writer keeps its own pytest cache and bytecode; only the shared caches move.
    observed = json.loads((tmp_path / "writer/attempt/result.md").read_text())
    assert observed["PYTEST_ADDOPTS"] == "-q" and observed["PYTHONPYCACHEPREFIX"] is None
    assert observed["UV_CACHE_DIR"] == str(Path(writer["run_dir"]) / "tmp/cache/uv")


@pytest.mark.parametrize("pattern", ["**", "*", "lock?", "[ab]", "{a,b}"])
def test_codex_read_only_never_grants_a_directory_named_like_a_pattern(tmp_path, pattern):
    repo = tmp_path / "repo"
    literal = repo / pattern
    literal.mkdir(parents=True)
    plan = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello", cwd=repo,
                                   workspace_root=repo, add_dirs=[str(literal)], network=False)
    assert codex_read_only_permissions(plan["argv"])[-1] == (
        'permissions.NAME.filesystem={":tmpdir" = "write", ' + json.dumps(provenant_state()) + ' = "write"}')
    assert any(warning.endswith(str(literal)) and "pattern character" in warning for warning in plan["warnings"])


@pytest.mark.live_provider
@pytest.mark.skipif(sys.platform != "darwin" or not shutil.which("codex"), reason="needs the Codex seatbelt")
def test_codex_read_only_directory_named_like_a_pattern_leaves_cwd_unwritable(tmp_path):
    repo = tmp_path / "repo"
    literal = repo / "**"
    literal.mkdir(parents=True)
    temp = tmp_path / "attempt/tmp"
    temp.mkdir(parents=True)
    (tmp_path / "codex-home").mkdir()
    plan = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello", cwd=repo,
                                   workspace_root=repo, add_dirs=[str(literal)], network=False)
    command = codex_sandbox_command(plan, repo)
    # Codex strips a trailing /** from a permission path, so granting <repo>/** would grant <repo>.
    probe = subprocess.run([*command, "--", "/bin/sh", "-c", f"touch '{repo}/escaped'"],
                           capture_output=True, text=True, timeout=60,
                           env={**os.environ, "TMPDIR": str(temp), "CODEX_HOME": str(tmp_path / "codex-home")})
    assert probe.returncode != 0
    assert not (repo / "escaped").exists()


def test_codex_writer_may_merge_away_the_last_instruction_file(tmp_path):
    repo, lane = instruction_lane(tmp_path)
    git(repo, "rm", "-q", "-r", ".agents")
    git(repo, "commit", "-q", "-m", "main drops its only skill")
    record = lane_attempt(tmp_path, lane, "git('merge', '-q', '--no-edit', '-X', 'theirs', 'main')\n")
    assert record["status"] == "ok", record
    assert not os.path.lexists(lane / ".agents")


def test_codex_writer_deleting_instructions_main_keeps_fails_under_deny_policy(tmp_path):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, "import shutil; shutil.rmtree('.agents')\n",
                          policy={"instruction_changes": "deny"})
    assert record["error"] == "protected_instructions_changed"
    assert any(SKILL in warning for warning in record["warnings"])


def fake_toolchain_home(tmp_path, monkeypatch):
    """A home holding toolchains in the usual places, plus credential stores beside them."""
    home = tmp_path / "home"
    monkeypatch.setenv("HOME", str(home))
    tools = {
        "node": home / ".nvm/versions/node/v24.0.0/bin/node",
        "python3": home / ".pyenv/versions/3.13.0/bin/python3.13",
        "uv": home / ".local/bin/uv",
    }
    for name, path in tools.items():
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(f"#!/bin/sh\necho {name}-ran\n")
        path.chmod(0o755)
    (home / ".nvm/versions/node/v24.0.0/lib/node_modules").mkdir(parents=True)
    (home / ".nvm/versions/node/v24.0.0/lib/runtime.js").write_text("runtime\n")
    (home / ".pyenv/versions/3.13.0/lib/python3.13").mkdir(parents=True)
    (home / ".pyenv/versions/3.13.0/include/python3.13").mkdir(parents=True)
    (home / ".pyenv/versions/3.13.0/include/python3.13/Python.h").write_text("header\n")
    # A sibling of the runtime directories: an install prefix grant would expose it.
    (home / ".pyenv/versions/3.13.0/auth.json").write_text("secret\n")
    (home / ".nvm/versions/node/v24.0.0/auth.json").write_text("secret\n")
    links = home / "links"
    links.mkdir()
    (links / "python3").symlink_to(tools["python3"])  # as pyenv-style or uv-style links are
    (home / ".ssh").mkdir()
    (home / ".ssh/id_ed25519").write_text("secret\n")
    (home / ".ssh/bin").mkdir()
    (home / ".ssh/lib/node_modules").mkdir(parents=True)
    (home / ".ssh/bin/node").write_text("#!/bin/sh\necho planted\n")
    (home / ".ssh/bin/node").chmod(0o755)
    (home / ".local/share/opencode").mkdir(parents=True)
    (home / ".local/share/opencode/auth.json").write_text("secret\n")
    (home / "notes.txt").write_text("private\n")
    # Hermetic: a host toolchain on /usr/bin (Linux runners ship python3 with /usr/lib/python3.*)
    # would add its own grants. A test that runs a shell appends the system directories itself.
    search_path = os.pathsep.join([str(links), str(tools["node"].parent), str(tools["uv"].parent)])
    return home, tools, search_path


def test_toolchain_reads_resolve_path_commands_to_their_runtime_components(tmp_path, monkeypatch):
    home, tools, search_path = fake_toolchain_home(tmp_path, monkeypatch)
    repo = tmp_path / "repo"
    repo.mkdir()
    plan = {"adapter": "claude", "mode": "read_only", "cwd": str(repo), "applied": {"add_dirs": []}}
    files, _, directories = supervisor().provider_confinement._toolchain_reads(plan, repo, search_path)
    node, python = home / ".nvm/versions/node/v24.0.0", home / ".pyenv/versions/3.13.0"
    assert tools["node"] in files and tools["python3"] in files
    assert sorted(directories) == sorted([node / "lib", python / "lib", python / "include"])
    # The prefix itself, and so a sibling like auth.json, is never granted.
    for prefix in (node, python):
        assert prefix not in directories
        assert not any((prefix / "auth.json").is_relative_to(granted) for granted in [*files, *directories])
    # ~/.local holds a credential store, so only the uv executable is granted.
    assert home / ".local/bin/uv" in files
    for granted in [*files, *directories]:
        assert granted != home and not supervisor().provider_confinement.credential_path(granted)
        assert not (home / ".local").is_relative_to(granted)


@pytest.mark.parametrize("selection", ["environment", "local", "global"])
def test_toolchain_reads_resolve_pyenv_shims_without_running_them(tmp_path, monkeypatch, selection):
    home, tools, _ = fake_toolchain_home(tmp_path, monkeypatch)
    monkeypatch.delenv("PYENV_VERSION", raising=False)
    monkeypatch.delenv("PYENV_DIR", raising=False)
    root = home / ".pyenv"
    monkeypatch.setenv("PYENV_ROOT", str(root))
    (root / "shims").mkdir()
    shim = root / "shims/python3"
    marker = tmp_path / "shim-ran"
    shim.write_text(f"#!/bin/sh\ntouch '{marker}'\nexit 1\n")
    shim.chmod(0o755)
    (tools["python3"].parent / "python3").symlink_to(tools["python3"])
    repo = tmp_path / "repo"
    cwd = repo / "sub"
    cwd.mkdir(parents=True)
    (root / "version").write_text("missing-version\n")
    if selection == "environment":
        monkeypatch.setenv("PYENV_VERSION", "3.13.0")
    elif selection == "local":
        (repo / ".python-version").write_text("3.13.0\n")
    else:
        (root / "version").write_text("3.13.0\n")
    plan = {"adapter": "claude", "mode": "read_only", "cwd": str(cwd), "applied": {"add_dirs": []}}
    files, _, directories = supervisor().provider_confinement._toolchain_reads(plan, repo, str(root / "shims"))
    assert tools["python3"] in files
    assert tools["python3"].parent.parent / "lib" in directories
    assert tools["python3"].parent.parent not in directories
    assert root not in directories
    assert not marker.exists()


@pytest.mark.parametrize("version", ["../../.ssh", "shim-link"])
def test_unsafe_pyenv_shim_fails_clearly(tmp_path, monkeypatch, version):
    home, tools, _ = fake_toolchain_home(tmp_path, monkeypatch)
    root = home / ".pyenv"
    monkeypatch.setenv("PYENV_ROOT", str(root))
    monkeypatch.setenv("PYENV_VERSION", "3.13.0" if version == "shim-link" else version)
    (root / "shims").mkdir()
    shim = root / "shims/python3"
    if version == "shim-link":
        credentials = home / ".ssh/id_ed25519"
        credentials.chmod(0o755)
        shim.symlink_to(credentials)
        (tools["python3"].parent / "python3").symlink_to(tools["python3"])
    else:
        shim.write_text("#!/bin/sh\nexit 1\n")
        shim.chmod(0o755)
    repo = tmp_path / "repo"
    repo.mkdir()
    plan = {"adapter": "claude", "mode": "read_only", "cwd": str(repo), "applied": {"add_dirs": []}}
    with pytest.raises(ValueError, match="pyenv shim.*fix:"):
        supervisor().provider_confinement._toolchain_reads(plan, repo, str(root / "shims"))


@pytest.mark.parametrize("version", ["missing-version", "3.13.0", None])
def test_unresolvable_pyenv_selection_is_skipped_with_a_warning(tmp_path, monkeypatch, caplog, version):
    home, tools, _ = fake_toolchain_home(tmp_path, monkeypatch)
    root = home / ".pyenv"
    monkeypatch.setenv("PYENV_ROOT", str(root))
    monkeypatch.delenv("PYENV_DIR", raising=False)
    if version is None:
        monkeypatch.delenv("PYENV_VERSION", raising=False)  # no file either: pyenv means "system"
    else:
        monkeypatch.setenv("PYENV_VERSION", version)
    (root / "shims").mkdir()
    shim = root / "shims/python3"
    shim.write_text("#!/bin/sh\nexit 1\n")
    shim.chmod(0o755)
    if version == "3.13.0":
        # An executable without a Python runtime layout must not earn a grant.
        shutil.rmtree(tools["python3"].parent.parent / "lib")
        (tools["python3"].parent / "python3").symlink_to(tools["python3"])
    repo = tmp_path / "repo"
    repo.mkdir()
    plan = {"adapter": "claude", "mode": "read_only", "cwd": str(repo), "applied": {"add_dirs": []}}
    files, _, _ = supervisor().provider_confinement._toolchain_reads(plan, repo, str(root / "shims"))
    assert shim.resolve() not in files
    if version:
        assert "skipping toolchain candidate" in caplog.text


def test_pyenv_prefix_selection_uses_the_highest_installed_patch(tmp_path, monkeypatch):
    home, tools, _ = fake_toolchain_home(tmp_path, monkeypatch)
    root = home / ".pyenv"
    monkeypatch.setenv("PYENV_ROOT", str(root))
    monkeypatch.setenv("PYENV_VERSION", "3.13")
    (root / "shims").mkdir()
    shim = root / "shims/python3"
    shim.write_text("#!/bin/sh\nexit 1\n")
    shim.chmod(0o755)
    (root / "versions/3.13.9").symlink_to(root / "versions/3.13.0")
    # 3.13.10 outranks 3.13.9 numerically, 3.130.0 is not a 3.13 patch.
    assert supervisor().provider_confinement._pyenv_version_dir(root, "3.13").name == "3.13.9"
    (root / "versions/3.13.10").symlink_to(root / "versions/3.13.0")
    (root / "versions/3.130.0").symlink_to(root / "versions/3.13.0")
    assert supervisor().provider_confinement._pyenv_version_dir(root, "3.13").name == "3.13.10"
    assert supervisor().provider_confinement._pyenv_version_dir(root, "3.13.0").name == "3.13.0"
    assert supervisor().provider_confinement._pyenv_version_dir(root, "3.99") is None


def test_unsafe_pyenv_shim_is_rejected_by_execute(tmp_path, monkeypatch):
    def refuse(*args, **kwargs):
        raise ValueError("cannot resolve pyenv shim x; fix: select an installed Python version")
    monkeypatch.setattr(supervisor().provider_confinement, "confinement_command", refuse)
    plan = fixture_plan(tmp_path, "pass", "claude")
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "rejected"
    assert "fix: select an installed Python version" in (record["evidence"]["excerpt"] + str(record))


def test_toolchain_reads_follow_a_workspace_venv_to_its_base_interpreter(tmp_path, monkeypatch):
    home, tools, _ = fake_toolchain_home(tmp_path, monkeypatch)
    repo = tmp_path / "repo"
    cwd = repo / "packages/app"
    cwd.mkdir(parents=True)
    git(tmp_path, "init", "-q", str(repo))
    (repo / ".venv/bin").mkdir(parents=True)
    (repo / ".venv/pyvenv.cfg").write_text(f"home = {tools['python3'].parent}\n")
    (repo / ".venv/bin/python").symlink_to(tools["python3"])
    plan = {"adapter": "claude", "mode": "read_only", "cwd": str(cwd), "applied": {"add_dirs": []}}
    files, project_files, directories = supervisor().provider_confinement._toolchain_reads(plan, repo, "/nonexistent")
    assert repo.resolve() / ".venv" in directories
    assert home / ".pyenv/versions/3.13.0/lib" in directories
    assert tools["python3"] in files
    assert not project_files  # no uv on PATH


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_confined_read_only_lane_runs_home_toolchains_but_not_credentials(tmp_path, monkeypatch):
    mod = supervisor()
    if not mod.provider_confinement._sandbox_exec_path():
        pytest.skip("sandbox-exec is unavailable or disabled")
    home, tools, toolchain_path = fake_toolchain_home(tmp_path, monkeypatch)
    search_path = os.pathsep.join([toolchain_path, "/usr/bin", "/bin"])
    repo = tmp_path / "repo"
    repo.mkdir()
    attempt = tmp_path / "attempt"
    attempt.mkdir()
    plan = {"adapter": "claude", "mode": "read_only", "cwd": str(repo), "workspace_root": str(repo),
            "run_dir": str(attempt), "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    profile = mod.provider_confinement.os_confinement_profile(plan, search_path)

    def run(command):
        return subprocess.run([mod.provider_confinement._sandbox_exec_path(), "-p", profile, "/bin/sh", "-c", command], cwd=repo,
                              capture_output=True, text=True, env={**os.environ, "PATH": search_path})

    probe = run("true")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert run("python3").stdout == "python3-ran\n"
    assert run("node && cat " + shlex.quote(str(home / ".nvm/versions/node/v24.0.0/lib/runtime.js"))).stdout \
        == "node-ran\nruntime\n"
    assert run("uv").stdout == "uv-ran\n"
    assert run("cat " + shlex.quote(str(home / ".pyenv/versions/3.13.0/include/python3.13/Python.h"))).stdout \
        == "header\n"
    for secret in (home / ".ssh/id_ed25519", home / ".local/share/opencode/auth.json", home / "notes.txt",
                   home / ".pyenv/versions/3.13.0/auth.json", home / ".nvm/versions/node/v24.0.0/auth.json"):
        assert run("cat " + shlex.quote(str(secret))).returncode != 0, secret
    # A tool in a credential store is not granted even when PATH names it.
    planted = mod.provider_confinement.os_confinement_profile(plan, str(home / ".ssh/bin"))
    blocked = subprocess.run([mod.provider_confinement._sandbox_exec_path(), "-p", planted, str(home / ".ssh/bin/node")],
                             capture_output=True, text=True)
    assert blocked.returncode != 0 and "planted" not in blocked.stdout


@pytest.mark.skipif(sys.platform != "darwin" or not shutil.which("uv"), reason="needs sandbox-exec and uv")
def test_confined_read_only_lane_runs_uv_with_a_home_interpreter(tmp_path):
    mod = supervisor()
    if not mod.provider_confinement._sandbox_exec_path():
        pytest.skip("sandbox-exec is unavailable or disabled")
    base = Path(sys.base_prefix).resolve()
    if not base.is_relative_to(Path.home().resolve()):
        pytest.skip("this interpreter is not installed under home")
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "pyproject.toml").write_text('[project]\nname = "probe"\nversion = "0"\nrequires-python = ">=3.9"\n'
                                         "dependencies = []\n")
    environment = {**os.environ, "UV_PYTHON": str(Path(sys.base_prefix) / "bin/python3"),
                   "UV_CACHE_DIR": str(tmp_path / "uv-cache")}
    environment.pop("VIRTUAL_ENV", None)
    subprocess.run(["uv", "sync", "--offline", "-q"], cwd=repo, env=environment, check=True, timeout=120)
    attempt = tmp_path / "attempt"
    (attempt / "tmp/cache").mkdir(parents=True)
    plan = {"adapter": "claude", "mode": "read_only", "cwd": str(repo), "workspace_root": str(repo),
            "run_dir": str(attempt), "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    lane_path = os.pathsep.join([str(Path(shutil.which("uv")).parent), "/usr/bin", "/bin"])
    profile = mod.provider_confinement.os_confinement_profile(plan, lane_path)
    result = subprocess.run(
        [mod.provider_confinement._sandbox_exec_path(), "-p", profile, "uv", "run", "--frozen", "--offline", "python", "-c", "print(1)"],
        cwd=repo, capture_output=True, text=True, timeout=120,
        env={**environment, "PATH": lane_path, "UV_CACHE_DIR": str(attempt / "tmp/cache/uv"),
             "TMPDIR": str(attempt / "tmp")},
    )
    if result.returncode and "sandbox_apply" in result.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert result.returncode == 0, result.stderr
    assert result.stdout == "1\n"
    ssh = Path.home() / ".ssh"
    if ssh.is_dir():
        listing = subprocess.run([mod.provider_confinement._sandbox_exec_path(), "-p", profile, "/bin/ls", str(ssh)],
                                 capture_output=True, text=True)
        assert listing.returncode != 0


def venv_repo(tmp_path, python_target=None, home_line=None):
    """A repository with a .venv whose interpreter link and pyvenv.cfg the repository controls."""
    repo = tmp_path / "repo"
    (repo / ".venv/bin").mkdir(parents=True)
    git(tmp_path, "init", "-q", str(repo))
    (repo / ".venv/pyvenv.cfg").write_text(f"home = {home_line}\n" if home_line else "version = 3.13\n")
    if python_target is not None:
        (repo / ".venv/bin/python").symlink_to(python_target)
    return repo, {"adapter": "claude", "mode": "read_only", "cwd": str(repo), "applied": {"add_dirs": []}}


def fake_executable(path, text="#!/bin/sh\necho ran\n"):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    path.chmod(0o755)
    return path


def granted_outside(repo, reads):
    files, _, directories = reads
    return [path for path in [*files, *directories] if not path.is_relative_to(repo.resolve())]


def test_pyvenv_home_naming_a_home_folder_grants_nothing(tmp_path, monkeypatch):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    fake_executable(home / "Documents/bin/python3")
    (home / "Documents/taxes.pdf").write_text("private\n")
    repo, plan = venv_repo(tmp_path, home_line=str(home / "Documents/bin"))
    assert granted_outside(repo, supervisor().provider_confinement._toolchain_reads(plan, repo, "/nonexistent")) == []


def test_venv_link_to_a_credential_file_grants_nothing(tmp_path, monkeypatch):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    secret = fake_executable(home / ".git-credentials", "https://user:token@example.invalid\n")
    repo, plan = venv_repo(tmp_path, python_target=secret)
    assert supervisor().provider_confinement.credential_path(secret)
    assert granted_outside(repo, supervisor().provider_confinement._toolchain_reads(plan, repo, "/nonexistent")) == []


def test_venv_link_to_a_non_toolchain_folder_grants_nothing(tmp_path, monkeypatch):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    target = fake_executable(home / "Documents/project/bin/python3")  # no lib/python3.* beside it
    repo, plan = venv_repo(tmp_path, python_target=target)
    assert granted_outside(repo, supervisor().provider_confinement._toolchain_reads(plan, repo, "/nonexistent")) == []


def test_interpreter_installed_directly_in_home_grants_nothing(tmp_path, monkeypatch):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    target = fake_executable(home / "bin/python3")
    (home / "lib/python3.13").mkdir(parents=True)
    repo, plan = venv_repo(tmp_path, python_target=target, home_line=str(home / "bin"))
    assert granted_outside(repo, supervisor().provider_confinement._toolchain_reads(plan, repo, str(home / "bin"))) == []


def test_venv_link_to_a_real_install_grants_its_runtime_components(tmp_path, monkeypatch):
    home, tools, _ = fake_toolchain_home(tmp_path, monkeypatch)
    repo, plan = venv_repo(tmp_path, python_target=tools["python3"], home_line=str(tools["python3"].parent))
    prefix = home / ".pyenv/versions/3.13.0"
    assert granted_outside(repo, supervisor().provider_confinement._toolchain_reads(plan, repo, "/nonexistent")) == [
        tools["python3"], prefix / "lib", prefix / "include"
    ]


@pytest.mark.parametrize("name", ["python3.14t", "python3t", "python3.13"])
def test_free_threaded_interpreter_is_a_toolchain(tmp_path, monkeypatch, name):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    prefix = home / ".local/share/uv/python/cpython-3.14.0t-macos-aarch64-none"
    interpreter = fake_executable(prefix / "bin" / name)
    (prefix / "lib/python3.14t").mkdir(parents=True)
    assert supervisor().provider_confinement._toolchain_grant(interpreter) == ([interpreter], [prefix / "lib"])


def fake_framework_python(home):
    """A framework build in home: bin/python3.13 loads <prefix>/Python and may re-exec Resources."""
    prefix = home / "Library/Frameworks/Python.framework/Versions/3.13"
    interpreter = fake_executable(prefix / "bin/python3.13")
    (prefix / "lib/python3.13").mkdir(parents=True)
    (prefix / "include/python3.13").mkdir(parents=True)
    (prefix / "Python").write_bytes(b"dylib")
    (prefix / "Resources/Python.app/Contents/MacOS").mkdir(parents=True)
    (prefix / "auth.json").write_text("secret\n")
    return prefix, interpreter


def test_framework_python_grants_its_library_and_resources_but_not_the_prefix(tmp_path, monkeypatch):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    prefix, interpreter = fake_framework_python(home)
    files, directories = supervisor().provider_confinement._toolchain_grant(interpreter)
    assert files == [interpreter, prefix / "Python"]
    assert directories == [prefix / "lib", prefix / "include", prefix / "Resources"]
    assert not any((prefix / "auth.json").is_relative_to(granted) for granted in [*files, *directories])


def test_framework_python_library_or_resources_linked_out_of_the_prefix_is_not_granted(tmp_path, monkeypatch):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    prefix, interpreter = fake_framework_python(home)
    (prefix / "Python").unlink()
    (prefix / "Python").symlink_to(home / ".ssh/id_ed25519")
    shutil.rmtree(prefix / "Resources")
    (prefix / "Resources").symlink_to(home / ".ssh")
    assert supervisor().provider_confinement._toolchain_grant(interpreter) == (
        [interpreter], [prefix / "lib", prefix / "include"])
    # Even a link that stays inside the prefix is left out: only a real entry is canonical.
    (prefix / "Python").unlink()
    (prefix / "lib/Python").write_bytes(b"dylib")
    (prefix / "Python").symlink_to(prefix / "lib/Python")
    assert supervisor().provider_confinement._toolchain_grant(interpreter)[0] == [interpreter]


@pytest.mark.parametrize("component", ["Resources", "include", "libexec"])
def test_runtime_directory_linked_to_the_prefix_itself_is_not_granted(tmp_path, monkeypatch, component):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    prefix, interpreter = fake_framework_python(home)
    if (prefix / component).exists():
        shutil.rmtree(prefix / component)
    (prefix / component).symlink_to(".")
    assert (prefix / component).resolve() == prefix
    files, directories = supervisor().provider_confinement._toolchain_grant(interpreter)
    assert prefix not in [*files, *directories]
    assert (prefix / component) not in directories
    assert not any((prefix / "auth.json").is_relative_to(granted) for granted in [*files, *directories])
    assert prefix / "lib" in directories


@pytest.mark.parametrize("install", ["pyenv", "framework"])
def test_lib_linked_to_the_prefix_itself_grants_nothing(tmp_path, monkeypatch, install):
    home, tools, _ = fake_toolchain_home(tmp_path, monkeypatch)
    if install == "framework":
        prefix, interpreter = fake_framework_python(home)
    else:
        prefix, interpreter = home / ".pyenv/versions/3.13.0", tools["python3"]
    shutil.rmtree(prefix / "lib")
    (prefix / "python3.13").mkdir()  # so lib/python3.13 still exists through the link
    (prefix / "lib").symlink_to(".")
    assert (prefix / "lib/python3.13").is_dir()
    assert supervisor().provider_confinement._toolchain_grant(interpreter) == ([], [])


def test_runtime_component_must_lie_strictly_inside_the_prefix(tmp_path):
    mod = supervisor()
    prefix = tmp_path / "prefix"
    (prefix / "lib").mkdir(parents=True)
    (prefix / "Python").write_bytes(b"dylib")
    (prefix / "Resources").symlink_to(".")
    (prefix / "include").symlink_to(tmp_path)
    assert mod.provider_confinement._runtime_component(prefix, "lib", directory=True) == prefix / "lib"
    assert mod.provider_confinement._runtime_component(prefix, "Python", directory=False) == prefix / "Python"
    for name in ("Resources", "include", ".", ""):
        assert mod.provider_confinement._runtime_component(prefix, name, directory=True) is None, name
    assert mod.provider_confinement._runtime_component(prefix, "lib", directory=False) is None


@pytest.mark.skipif(sys.platform != "darwin" or not all(map(shutil.which, ("install_name_tool", "codesign", "otool"))),
                    reason="needs sandbox-exec and the macOS binary tools")
def test_confined_read_only_lane_runs_a_framework_python_in_home(tmp_path, monkeypatch):
    mod = supervisor()
    if not mod.provider_confinement._sandbox_exec_path():
        pytest.skip("sandbox-exec is unavailable or disabled")
    source = next((path.resolve() for path in sorted(Path("/opt/homebrew/opt").glob(
        "python@3.*/Frameworks/Python.framework/Versions/3.*")) if (path / "Python").is_file()), None)
    if source is None:
        pytest.skip("no Homebrew framework Python to clone")
    home = tmp_path / "home"
    monkeypatch.setenv("HOME", str(home))
    prefix = home / "Library/Frameworks/Python.framework/Versions" / source.name
    prefix.parent.mkdir(parents=True)
    subprocess.run(["cp", "-cR", str(source), str(prefix)], check=True)  # an APFS clone, not a copy
    interpreter = prefix / "bin" / ("python" + source.name)
    linked = subprocess.run(["otool", "-L", str(interpreter)], capture_output=True, text=True,
                            check=True).stdout.splitlines()[1].split()[0]
    # Load the clone's own library, so launch needs the home framework's Python file.
    subprocess.run(["install_name_tool", "-change", linked, str(prefix / "Python"), str(interpreter)],
                   check=True, capture_output=True)
    subprocess.run(["codesign", "-f", "-s", "-", str(interpreter)], check=True, capture_output=True)
    links = home / "links"
    links.mkdir()
    (links / "python3").symlink_to(interpreter)
    (prefix / "auth.json").write_text("secret\n")
    repo = tmp_path / "repo"
    repo.mkdir()
    attempt = tmp_path / "attempt"
    attempt.mkdir()
    plan = {"adapter": "claude", "mode": "read_only", "cwd": str(repo), "workspace_root": str(repo),
            "run_dir": str(attempt), "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    search_path = os.pathsep.join([str(links), "/usr/bin", "/bin"])
    profile = mod.provider_confinement.os_confinement_profile(plan, search_path)

    def run(*command):
        return subprocess.run([mod.provider_confinement._sandbox_exec_path(), "-p", profile, *command], cwd=repo, capture_output=True,
                              text=True, timeout=60, env={**os.environ, "PATH": search_path})

    started = run("python3", "-c", "print('framework-ran')")
    if started.returncode and "sandbox_apply" in started.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert started.stdout == "framework-ran\n", started.stderr[-400:]
    assert run("/bin/cat", str(prefix / "auth.json")).returncode != 0


def test_linked_runtime_directory_is_not_granted(tmp_path, monkeypatch):
    home, tools, _ = fake_toolchain_home(tmp_path, monkeypatch)
    prefix = home / ".pyenv/versions/3.13.0"
    shutil.rmtree(prefix / "include")
    (prefix / "include").symlink_to(home / ".ssh")
    assert supervisor().provider_confinement._toolchain_grant(tools["python3"]) == ([tools["python3"]], [prefix / "lib"])


def test_toolchain_rules_emit_the_validated_path_without_resolving_again(tmp_path):
    mod = supervisor()
    target = tmp_path / "elsewhere"
    target.mkdir()
    link = tmp_path / "lib"
    link.symlink_to(target)
    assert mod.provider_confinement._sbpl_rule("allow", "file-read-data", [link], canonical=True) \
        == f'(allow file-read-data (subpath "{link}"))\n'
    assert mod.provider_confinement._sbpl_rule("allow", "file-read-data", [link], literal=True, canonical=True) \
        == f'(allow file-read-data (literal "{link}"))\n'
    assert str(target.resolve()) in mod.provider_confinement._sbpl_rule("allow", "file-read-data", [link])


@pytest.mark.parametrize("secret", [
    ".git-credentials", ".netrc", ".npmrc", ".pypirc", ".pgpass", ".vault-token", ".docker/config.json",
    ".kube/config", ".aws/credentials", ".config/gh/hosts.yml", ".config/gcloud/credentials.db",
    ".azure/accessTokens.json", ".gnupg/private-keys-v1.d", ".ssh/id_ed25519", ".password-store/a.gpg",
    ".cargo/credentials.toml", ".gem/credentials", ".config/git/credentials", ".terraform.d/credentials.tfrc.json",
    ".boto", ".s3cfg", ".config/hub", ".config/op/config",
])
def test_credential_path_covers_common_host_secrets(tmp_path, monkeypatch, secret):
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    assert supervisor().provider_confinement.credential_path(tmp_path / "home" / secret)


def test_failed_writer_with_instruction_edits_is_not_usable(tmp_path):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, f"open({SKILL!r}, 'w').write('lane edit\\n')\n"
                          "import sys; sys.exit(3)\n", policy={"instruction_changes": "deny"})
    assert record["status"] == "failed"
    assert record["exit"] == 3
    assert record["landable"] is False


def test_failed_instruction_restoration_fails_but_keeps_output_and_patch(tmp_path):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, f"open({SKILL!r}, 'w').write('staged edit\\n'); git('add', {SKILL!r})\n"
                          f"open({SKILL!r}, 'w').write('lane edit\\n')\n"
                          "directory = git('rev-parse', '--absolute-git-dir')\n"
                          "open(directory + '/index.lock', 'w').close()\n")
    assert record["status"] == "failed"
    assert record["error"] == "protected_instructions_changed"
    assert "restore .agents/ to the start commit" in record["fix"]
    assert record["landable"] is False
    assert record["quarantine"]["restored"] is False
    assert record["quarantine"]["patch_error"]
    assert 'lane edit' in (tmp_path / 'attempt/protected.patch').read_text()
    assert str(tmp_path / 'attempt/protected.index.patch') in record['quarantine']['patches']
    assert Path(record["output_path"]).read_text() == 'done'


def _stderr_run(tmp_path, lines, delays=0.05):
    code = "import sys,time\n" + "".join(
        f"sys.stderr.write({line + chr(10)!r});sys.stderr.flush();time.sleep({delays})\n" for line in lines
    ) + "time.sleep(30)\n"
    plan = fixture_plan(tmp_path, code, "opencode", startup_seconds=0.6, timeout_seconds=3)
    plan["model"] = "opencode-go/glm-5"
    return supervisor().execute(plan, tmp_path / "result.md")


@pytest.mark.parametrize("lines", [
    ["WARN retrying: Individual quota reached"],
    ["WARN retrying: Individual quota reached", "ERROR service=bus unrelated failure"],
    ["WARN retrying: Individual quota reached, resets in 30s",
     "ERROR service=bus unrelated failure"],
], ids=["quota-warning", "unrelated-error", "reset-hint"])
def test_opencode_quota_warning_noise_is_not_fatal(tmp_path, lines):
    record = _stderr_run(tmp_path, lines)
    assert record["status"] != "usage_limited"


def test_opencode_reset_hint_on_a_separate_earlier_line_is_kept(tmp_path):
    record = _stderr_run(tmp_path, ["Resets in 2 days.", "ERROR error=Monthly usage limit reached."])
    assert record["status"] == "usage_limited"
    assert record["retry_after"] is None
    assert record["reset_at"]


def test_opencode_explicit_period_limit_phrase_is_fatal_without_error_prefix(tmp_path):
    record = _stderr_run(tmp_path, ["retrying: weekly usage limit reached"])
    assert record["status"] == "usage_limited"


def test_explicit_model_runs_despite_a_cooldown_with_a_warning(tmp_path):
    bindir = tmp_path / "bin"
    bindir.mkdir()
    cli = bindir / "claude"
    cli.write_text("#!/usr/bin/env python3\nimport json,sys\nsys.stdin.read()\n"
                   "print(json.dumps({'type':'result','is_error':False,'result':'DONE'}))\n")
    cli.chmod(0o755)
    until = (datetime.now(UTC) + timedelta(hours=2)).strftime("%Y-%m-%dT%H:%M:%SZ")
    (tmp_path / "cooldowns.json").write_text(json.dumps({"schema": "fabric.cooldowns.v1", "cooldowns": {
        "claude/*": {"adapter": "claude", "model": "*", "account": None, "status": "usage_limited",
                     "cooling_until": until, "source_run": "r", "recorded_at": until, "signature": "quota_exhausted"}}}))
    # An account-less cooldown belongs to the default identity, so the lane must not select another one.
    env = {**{k: v for k, v in os.environ.items() if k != "CLAUDE_CONFIG_DIR"},
           "PROVENANT_NO_OS_CONFINEMENT": "1", "PATH": str(bindir) + ":" + os.environ["PATH"],
           "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT), "AGENT_FABRIC_INSTANCE_ROOT": str(ROOT),
           "FABRIC_COOLDOWNS_PATH": str(tmp_path / "cooldowns.json")}
    run = Path(subprocess.check_output([str(SCRIPTS / "run_dir_init.sh"), "--kind", "dispatch"],
                                       cwd=tmp_path, text=True).strip())
    (tmp_path / "prompt.md").write_text("Reply DONE")
    result = subprocess.run([sys.executable, str(SCRIPTS / "dispatch_run.py"), "--run-dir", str(run), "--tool", "claude",
                             "--model", "opus", "--prompt-file", str(tmp_path / "prompt.md")],
                            cwd=tmp_path, env=env, capture_output=True, text=True, timeout=15)
    assert result.returncode == 0, result.stdout + result.stderr
    row = json.loads(next((run / "tasks").glob("*/attempt-*/attempt.json")).read_text())
    assert row["status"] == "ok"
    assert any("explicit model is cooling" in warning for warning in row.get("warnings", []))


def test_opencode_v2_structured_error_log_lines_are_fatal_and_info_is_not():
    fatal = supervisor().provider_confinement.OPENCODE_FATAL_LOG
    quiet = supervisor().provider_confinement.OPENCODE_QUIET_LOG
    assert re.search(fatal, "timestamp=2026-10-16T07:38:10.410Z level=ERROR run=ab message=boom", re.I | re.M)
    assert not re.search(fatal, 'timestamp=2026-10-16T07:38:10.410Z level=INFO run=ab message="cli starting"', re.I | re.M)
    assert re.match(quiet, "timestamp=x level=WARN message=y")


def test_opencode_v1_cli_is_a_typed_refusal_on_the_provider_exec_cli(tmp_path, monkeypatch, capsys):
    mod = supervisor()
    monkeypatch.setattr(mod.provider_confinement, "_opencode_version_error",
                        lambda *_: "OpenCode 1.14.2 is unsupported; fix: upgrade OpenCode to v2")
    with pytest.raises(mod.CapabilityError) as caught:
        mod.build_plan("opencode", {"resolved_model": "opencode/x"}, "hello", cwd=tmp_path, workspace_root=tmp_path)
    assert caught.value.code == "unsupported_cli_version"
    assert isinstance(caught.value, ValueError)


def test_codex_toml_overrides_spell_non_bmp_names_as_toml():
    import tomllib
    codex = importlib.import_module("skills.orchestrate.scripts.adapters.codex")
    table = codex._toml_table({"\U0001F600": {"enabled": False}, "dot.name": {"enabled": False}})
    assert "\\ud83d" not in table
    assert tomllib.loads("mcp_servers = " + table)["mcp_servers"] == {
        "\U0001F600": {"enabled": False}, "dot.name": {"enabled": False},
    }


@pytest.mark.parametrize("adapter", ["claude", "cursor", "agy", "kiro"])
def test_provider_state_symlink_home_into_credential_store_is_refused(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    (home / ".ssh").mkdir(parents=True)
    relative = ".gemini" if adapter == "agy" else f".{adapter}"
    (home / relative).symlink_to(home / ".ssh", target_is_directory=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    plan = {"adapter": adapter, "mode": "read_only", "cwd": str(tmp_path),
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    with pytest.raises(PermissionError, match="credential store"):
        mod.provider_confinement.os_confinement_profile(plan)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    with pytest.raises(PermissionError, match="credential store"):
        mod.provider_confinement.confinement_command(plan, ["/usr/bin/true"])
    assert list((home / ".ssh").iterdir()) == []


@pytest.mark.parametrize("adapter", ["claude", "cursor", "agy", "kiro", "opencode", "codex"])
@pytest.mark.parametrize("mode", ["read_only", "worktree_write"])
@pytest.mark.parametrize("layout", ["existing", "absent", "symlink"])
@pytest.mark.parametrize("sandboxed", [False, True], ids=["profile", "sandbox-exec"])
def test_claude_json_is_read_only_to_every_lane(monkeypatch, tmp_path, adapter, mode, layout, sandboxed):
    mod = supervisor()
    home = (tmp_path / "home").resolve()
    workspace = home / "repo"
    workspace.mkdir(parents=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    config = home / ".claude.json"
    target = home / "dotfiles/claude-config.json" if layout == "symlink" else config
    target.parent.mkdir(parents=True, exist_ok=True)
    if layout != "absent":
        target.write_text("synthetic config")
    if layout == "symlink":
        config.symlink_to(target)
    plan = {"adapter": adapter, "mode": mode, "cwd": str(workspace), "run_dir": str(workspace),
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    profile = mod.provider_confinement.os_confinement_profile(plan)
    # A final deny also protects config when a broad writable root encompasses home or its target.
    for path in {config, target}:
        path_filter = mod.provider_confinement._sbpl_filter(str(path) + "*")
        rule = next(line for line in profile.splitlines()
                    if line.startswith("(deny file-write* ") and path_filter in line)
        assert profile.rfind(rule) > profile.rfind("(allow file-write*")
    if adapter == "claude" and mode == "read_only" and layout == "symlink":
        assert any(line.startswith("(allow file-read-data ") and mod.provider_confinement._sbpl_filter(target) in line
                   for line in profile.splitlines())
    if not sandboxed:
        return
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    command = [sandbox_exec, "-p", profile, "/bin/sh", "-c"]
    for path in {config, target}:
        for suffix in ("", ".tmp", ".backup"):
            result = subprocess.run([*command, f"printf bad > {shlex.quote(str(path) + suffix)}"],
                                    check=False, capture_output=True, text=True)
            assert result.returncode != 0, (path, suffix)
        source = workspace / "replacement"
        source.write_text("replacement")
        assert subprocess.run([*command, f"mv {shlex.quote(str(source))} {shlex.quote(str(path))}"],
                              check=False, capture_output=True, text=True).returncode != 0
    if layout != "absent":
        assert target.read_text() == "synthetic config"
        if adapter == "claude":
            read = subprocess.run([*command, f"cat {shlex.quote(str(config))}"], check=False, capture_output=True, text=True)
            assert read.returncode == 0, read.stderr
            assert read.stdout == "synthetic config"


@pytest.mark.parametrize("adapter", ["claude", "cursor", "agy", "kiro"])
def test_receipt_provider_state_grants_match_profile_with_symlink_home(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    # Exercise the default home rather than the suite's isolated Claude identity.
    monkeypatch.delenv("CLAUDE_CONFIG_DIR", raising=False)
    home = (tmp_path / "home").resolve()
    home.mkdir()
    target = (tmp_path / "dotfiles" / adapter).resolve()
    target.mkdir(parents=True)
    relative = ".gemini" if adapter == "agy" else f".{adapter}"
    (home / relative).symlink_to(target, target_is_directory=True)
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    force_sandbox(monkeypatch, "/usr/bin/sandbox-exec")
    monkeypatch.setattr(mod, "_require_kiro_engine", lambda: None)
    workspace = tmp_path / "repo"
    workspace.mkdir()
    plan = mod.build_plan(adapter, {}, "hello", cwd=workspace, run_dir=tmp_path / "attempt")
    profile = mod.provider_confinement.os_confinement_profile(plan)
    grants = plan["applied"]["write_boundary"]["writable_paths"]
    assert not any(str(home / relative) in path for path in grants)
    if adapter in {"claude", "cursor", "agy"}:
        assert any(str(target) in path for path in grants)
    for line in profile.splitlines():
        if not line.startswith("(allow file-write* "):
            continue
        for kind, value in re.findall(r'\((subpath|literal|regex) #?"([^"\n]+)"\)', line):
            if kind == "regex":
                assert "regex:" + value in grants, value
            else:
                assert value in grants, value


@pytest.mark.parametrize("adapter", ["claude", "cursor", "agy", "kiro"])
def test_provider_state_accepts_normal_provider_under_symlink_home(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    home = (tmp_path / "real-home").resolve()
    home.mkdir()
    alias = tmp_path / "home-alias"
    alias.symlink_to(home, target_is_directory=True)
    monkeypatch.setattr(mod.Path, "home", lambda: alias)
    plan = {"adapter": adapter, "mode": "read_only", "cwd": str(tmp_path),
            "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    assert mod.provider_confinement.os_confinement_profile(plan).startswith("(version 1)")
    relative = ".gemini" if adapter == "agy" else f".{adapter}"
    assert plan["provider_home"] == str(home / relative)


def protection(cwd, *extra):
    return subprocess.run([sys.executable, str(SCRIPTS / "lane_protection.py"), "--cwd", str(cwd), *extra],
                          capture_output=True, text=True, timeout=30)


def landing(repo, tip, base):
    """Fabric's landing gate: the leased remote SHA is the base, never a local ref."""
    return protection(repo, "--common-dir", str(repo / ".git"), "--tip", tip, "--base", base)


def side_commit(cwd, parent, content, path=SKILL):
    """A plumbing commit on parent with path replaced, leaving HEAD and the index alone."""
    environment = {**os.environ, "GIT_INDEX_FILE": str(Path(cwd) / ".side-index")}

    def run(*args, stdin=""):
        return subprocess.run(["git", "-C", str(cwd), *GIT_FIXTURE, *args], check=True, capture_output=True,
                              text=True, env=environment, input=stdin).stdout.strip()

    run("read-tree", parent)
    blob = run("hash-object", "-w", "--stdin", stdin=content)
    run("update-index", "--add", "--cacheinfo", f"100644,{blob},{path}")
    tree = run("write-tree")
    os.unlink(Path(cwd) / ".side-index")
    return run("commit-tree", tree, "-p", parent, "-m", "side")


def head_of(cwd, rev="HEAD"):
    return git(cwd, "rev-parse", rev).strip()


@pytest.mark.parametrize("ref", ["refs/heads/main", "refs/remotes/origin/main"])
def test_landing_refuses_protected_commits_laundered_through_a_moved_integration_ref(tmp_path, ref):
    repo, lane = instruction_lane(tmp_path)
    leased = head_of(repo, "main")
    # Writers can no longer move these refs; the gate must not rely on that either.
    poison = side_commit(lane, leased, "POISON\n")
    git(repo, "update-ref", ref, poison)
    git(lane, "merge", "-q", "--no-edit", ref)
    tip = head_of(lane)
    refused = landing(repo, tip, leased)
    assert refused.returncode == 1
    assert refused.stderr.startswith("protected_instructions_changed: unreleased commits")
    assert poison in refused.stderr and "quarantine" not in refused.stderr
    assert refused.stderr.count("protected_instructions_changed") == 1
    # A commit on the primary branch itself is checked the same way.
    assert landing(repo, poison, leased).returncode == 1


def test_landing_checks_the_landed_history_not_the_lane_branch(tmp_path):
    repo, lane = instruction_lane(tmp_path)
    leased = head_of(repo, "main")
    # The lane branch is rewritten onto main with a protected change, then consumed.
    git(lane, "reset", "-q", "--hard", side_commit(lane, leased, "POISON\n"))
    git(repo, "merge", "-q", "--no-ff", "-m", "consume lane", "lane")
    tip = head_of(repo)
    git(lane, "reset", "-q", "--hard", leased)  # The child then resets its branch.
    assert landing(repo, tip, leased).returncode == 1


def test_merging_the_trusted_base_into_a_lane_passes_but_rolling_it_back_does_not(tmp_path):
    repo, lane = instruction_lane(tmp_path)
    leased = head_of(repo, "main")
    # The chair refreshes the lane from main, which carries a trusted skill change.
    git(lane, "merge", "-q", "--no-edit", "main")
    assert landing(repo, head_of(lane), leased).returncode == 0
    checked = protection(lane)
    assert checked.returncode == 0, checked.stderr
    assert checked.stdout.strip() == head_of(lane)
    (lane / SKILL).write_text("v1\n")
    git(lane, "commit", "-q", "-am", "roll the skill back")
    assert landing(repo, head_of(lane), leased).returncode == 1


def test_a_registered_worktree_that_was_never_dispatched_lands_without_any_record(tmp_path):
    repo, lane = instruction_lane(tmp_path)
    assert not (repo / ".git/provenant-instruction-checks").exists()
    checked = protection(lane)
    assert checked.returncode == 0, checked.stderr
    assert landing(repo, head_of(lane), head_of(repo, "main")).returncode == 0


@pytest.mark.parametrize("history", ["revert", "evil-merge", "case-variant"])
def test_landing_refuses_each_commit_that_changes_instructions(tmp_path, history):
    repo, lane = instruction_lane(tmp_path)
    git(lane, "merge", "-q", "--no-edit", "main")
    leased = head_of(repo, "main")
    if history == "revert":
        (lane / SKILL).write_text("lane\n")
        git(lane, "commit", "-q", "-am", "edit")
        git(lane, "revert", "--no-edit", "HEAD")
    elif history == "evil-merge":
        # Neither parent changes the skill; only the merge resolution does.
        git(lane, "branch", "-q", "side", side_commit(lane, head_of(lane), "side\n", path="side.txt"))
        git(lane, "merge", "-q", "--no-ff", "--no-commit", "side")
        (lane / SKILL).write_text("merged\n")
        git(lane, "commit", "-q", "-am", "merge side")
    else:
        git(lane, "reset", "-q", "--hard", side_commit(lane, head_of(lane), "x\n", path=".AGENTS/x.md"))
    assert landing(repo, head_of(lane), leased).returncode == 1


def test_release_requires_the_reviewed_tip_and_covers_nothing_after_it(tmp_path, monkeypatch):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, lane = instruction_lane(tmp_path)
    leased = head_of(repo, "main")
    git(lane, "merge", "-q", "--no-edit", "main")
    (lane / SKILL).write_text("chair reviewed\n")
    git(lane, "commit", "-q", "-am", "reviewed edit")
    reviewed = head_of(lane)
    assert landing(repo, reviewed, leased).returncode == 1
    missing = protection(lane, "--release", "--actor", "chair", "--reason", "ok")
    assert missing.returncode == 1 and "--tip" in missing.stderr
    # A detached child races the chair with one more protected commit.
    (lane / SKILL).write_text("UNREVIEWED\n")
    git(lane, "commit", "-q", "-am", "race")
    raced = head_of(lane)
    moved = protection(lane, "--release", "--tip", reviewed, "--actor", "chair", "--reason", "ok")
    assert moved.returncode == 1 and "not the reviewed tip" in moved.stderr
    git(lane, "reset", "-q", "--hard", reviewed)
    released = protection(lane, "--release", "--tip", reviewed, "--actor", "chair", "--reason", "decision 7")
    assert released.returncode == 0, released.stderr
    [record] = (repo / ".git/provenant-instruction-checks/releases").glob("*.json")
    audit = json.loads(record.read_text())
    assert audit["tip"] == reviewed and audit["actor"] == "chair" and audit["reason"] == "decision 7"
    assert audit["worktree"] == str(lane.resolve())
    assert landing(repo, reviewed, leased).returncode == 0
    assert landing(repo, raced, leased).returncode == 1
    inside = subprocess.run([sys.executable, str(SCRIPTS / "lane_protection.py"), "--cwd", str(lane), "--release",
                             "--tip", reviewed, "--actor", "chair", "--reason", "ok"], capture_output=True, text=True,
                            env={**os.environ, "PROVENANT_ATTEMPT_MARKER": "1"})
    assert inside.returncode == 1


OTHER_SKILL = ".agents/skills/other/SKILL.md"


def released_train(repo, lane):
    """Release a lane tip that adds a skill, then land it on main with a train's --no-ff merge."""
    (lane / OTHER_SKILL).parent.mkdir(parents=True)
    (lane / OTHER_SKILL).write_text("reviewed\n")
    git(lane, "add", OTHER_SKILL)
    git(lane, "commit", "-q", "-m", "reviewed skill")
    released = protection(lane, "--release", "--tip", head_of(lane), "--actor", "chair", "--reason", "ok")
    assert released.returncode == 0, released.stderr


def test_a_landing_train_merge_of_a_released_tip_passes_and_so_do_later_merges(tmp_path, monkeypatch):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, lane = instruction_lane(tmp_path)
    leased = head_of(repo, "main")
    released_train(repo, lane)
    git(repo, "merge", "-q", "--no-ff", "-m", "Merge #1", "lane")
    checked = landing(repo, head_of(repo), leased)
    assert checked.returncode == 0, checked.stderr
    # A later pull request cut before the release lands on top without touching .agents/.
    later = side_commit(repo, head_of(repo, "main~2"), "later\n", path="later.txt")
    git(repo, "merge", "-q", "--no-ff", "-m", "Merge #2", later)
    checked = landing(repo, head_of(repo), leased)
    assert checked.returncode == 0, checked.stderr


@pytest.mark.parametrize("variant", ["identical", "one-byte", "extra-entry", "missing-entry", "mode"])
def test_a_landing_train_remerge_requires_the_complete_released_protected_tree(tmp_path, monkeypatch, variant):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, lane = instruction_lane(tmp_path)
    leased = head_of(repo, "main")
    # The reviewed merge resolution introduces content neither parent carries.
    git(repo, "merge", "-q", "--no-ff", "--no-commit", "lane")
    (repo / SKILL).write_text("reviewed resolution\n")
    (repo / OTHER_SKILL).parent.mkdir(parents=True)
    (repo / OTHER_SKILL).write_text("reviewed\n")
    git(repo, "add", OTHER_SKILL)
    git(repo, "commit", "-q", "-am", "M1")
    reviewed = head_of(repo)
    released = protection(repo, "--release", "--tip", reviewed, "--actor", "chair", "--reason", "ok")
    assert released.returncode == 0, released.stderr

    git(repo, "reset", "-q", "--hard", leased)
    git(repo, "merge", "-q", "--no-ff", "--no-commit", "lane")
    (repo / SKILL).write_text("reviewed resolution\n")
    (repo / OTHER_SKILL).parent.mkdir(parents=True)
    (repo / OTHER_SKILL).write_text("reviewed\n")
    git(repo, "add", OTHER_SKILL)
    if variant == "one-byte":
        (repo / SKILL).write_text("reviewed resolutioN\n")
    elif variant == "extra-entry":
        extra = ".agents/extra.md"
        (repo / extra).write_text("unreviewed\n")
        git(repo, "add", extra)
    elif variant == "missing-entry":
        git(repo, "rm", "-q", "-f", OTHER_SKILL)
    elif variant == "mode":
        git(repo, "update-index", "--chmod=+x", SKILL)
        (repo / SKILL).chmod(0o755)
    git(repo, "commit", "-q", "-am", "M2")
    merged = head_of(repo)
    assert merged != reviewed
    assert bool(git(repo, "diff", reviewed, merged, "--", ".agents/")) == (variant != "identical")
    checked = landing(repo, merged, leased)
    if variant == "identical":
        assert checked.returncode == 0, checked.stderr
    else:
        assert checked.returncode == 1
        assert merged in checked.stderr
        assert checked.stderr.startswith("protected_instructions_changed:")


def test_released_protected_trees_cannot_be_combined_path_by_path(tmp_path, monkeypatch):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, _ = instruction_lane(tmp_path)
    leased = head_of(repo)
    first = side_commit(repo, leased, "reviewed first\n")
    second = side_commit(repo, leased, "reviewed second\n", path=OTHER_SKILL)
    for tip in (first, second):
        git(repo, "reset", "-q", "--hard", tip)
        released = protection(repo, "--release", "--tip", tip, "--actor", "chair", "--reason", "ok")
        assert released.returncode == 0, released.stderr
    # Each path matches a release, but neither release reviewed the combination.
    combined = side_commit(repo, first, "reviewed second\n", path=OTHER_SKILL)
    refused = landing(repo, combined, leased)
    assert refused.returncode == 1
    assert combined in refused.stderr
    assert refused.stderr.startswith("protected_instructions_changed:")


def test_an_already_landed_release_cannot_vouch_for_a_reverting_merge(tmp_path, monkeypatch):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, _ = instruction_lane(tmp_path)
    leased = head_of(repo)
    old = head_of(repo, "main~1")
    git(repo, "reset", "-q", "--hard", old)
    released = protection(repo, "--release", "--tip", old, "--actor", "chair", "--reason", "ok")
    assert released.returncode == 0, released.stderr
    # Base has moved past the released v1 skill. A new merge must not restore it.
    later = side_commit(repo, leased, "later\n", path="later.txt")
    reverted = side_commit(repo, later, "v1\n")
    tree = head_of(repo, reverted + "^{tree}")
    merge = git(repo, "commit-tree", tree, "-p", leased, "-p", later, "-m", "revert in merge").strip()
    refused = landing(repo, merge, leased)
    assert refused.returncode == 1
    assert merge in refused.stderr
    assert refused.stderr.startswith("protected_instructions_changed:")


def test_a_release_with_the_bases_protected_tree_does_not_cover_new_content(tmp_path, monkeypatch):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, _ = instruction_lane(tmp_path)
    leased = head_of(repo)
    same_as_base = side_commit(repo, leased, "release with unchanged instructions\n", path="app.txt")
    git(repo, "reset", "-q", "--hard", same_as_base)
    released = protection(repo, "--release", "--tip", same_as_base, "--actor", "chair", "--reason", "ok")
    assert released.returncode == 0, released.stderr
    # Taking the base remains allowed even though the release contains no new protected content.
    restored = side_commit(repo, head_of(repo, "lane"), "v2\n")
    checked = landing(repo, restored, leased)
    assert checked.returncode == 0, checked.stderr
    unreviewed = side_commit(repo, restored, "unreviewed\n", path=OTHER_SKILL)
    refused = landing(repo, unreviewed, leased)
    assert refused.returncode == 1
    assert unreviewed in refused.stderr
    assert refused.stderr.startswith("protected_instructions_changed:")


@pytest.mark.parametrize("landed", ["superseded", "squashed"])
def test_a_stale_release_cannot_restore_protected_content_the_base_moved_past(tmp_path, monkeypatch, landed):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, _ = instruction_lane(tmp_path)
    fork = head_of(repo)
    reviewed = side_commit(repo, fork, "reviewed\n", path=OTHER_SKILL)
    git(repo, "reset", "-q", "--hard", reviewed)
    released = protection(repo, "--release", "--tip", reviewed, "--actor", "chair", "--reason", "ok")
    assert released.returncode == 0, released.stderr
    base = fork
    if landed == "squashed":
        # A squash lands the release's content without its SHA ever reaching the base's history.
        base = git(repo, "commit-tree", head_of(repo, reviewed + "^{tree}"), "-p", fork, "-m", "squash").strip()
    newer = side_commit(repo, base, "v3\n")
    # One commit on the newer base takes the release's whole tree, reverting SKILL from v3.
    stale = git(repo, "commit-tree", head_of(repo, reviewed + "^{tree}"), "-p", newer, "-m", "stale").strip()
    refused = landing(repo, stale, newer)
    assert refused.returncode == 1
    assert stale in refused.stderr
    assert refused.stderr.startswith("protected_instructions_changed:")


def test_a_release_with_unrelated_history_is_ignored_rather_than_blocking_checks(tmp_path, monkeypatch):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, _ = instruction_lane(tmp_path)
    leased = head_of(repo)
    empty = git(repo, "hash-object", "-t", "tree", "-w", "/dev/null").strip()
    orphan = git(repo, "commit-tree", empty, "-m", "orphan").strip()
    git(repo, "reset", "-q", "--hard", orphan)
    released = protection(repo, "--release", "--tip", orphan, "--actor", "chair", "--reason", "ok")
    assert released.returncode == 0, released.stderr
    # Taking the base's version still passes; an unreviewed change is still held.
    restored = side_commit(repo, head_of(repo, "lane"), "v2\n")
    assert landing(repo, restored, leased).returncode == 0
    unreviewed = side_commit(repo, restored, "unreviewed\n", path=OTHER_SKILL)
    refused = landing(repo, unreviewed, leased)
    assert refused.returncode == 1
    assert refused.stderr.startswith("protected_instructions_changed:")


@pytest.mark.parametrize("history", ["merge-edits-a-skill", "unreleased-lane", "stale-branch-revert",
                                     "ancestor-parent-revert"])
def test_a_landing_train_merge_still_holds_protected_content_no_released_parent_has(
    tmp_path, monkeypatch, history,
):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, lane = instruction_lane(tmp_path)
    leased = head_of(repo, "main")
    if history == "merge-edits-a-skill":
        released_train(repo, lane)
        git(repo, "merge", "-q", "--no-ff", "--no-commit", "lane")
        (repo / OTHER_SKILL).write_text("resolved by the merge\n")
        git(repo, "commit", "-q", "-am", "Merge #1")
        held = head_of(repo)
    elif history == "unreleased-lane":
        (lane / SKILL).write_text("unreviewed\n")
        git(lane, "commit", "-q", "-am", "unreviewed skill")
        held = head_of(lane)
        git(repo, "merge", "-q", "--no-ff", "-X", "theirs", "-m", "Merge #1", "lane")
    else:
        released_train(repo, lane)
        git(repo, "merge", "-q", "--no-ff", "-m", "Merge #1", "lane")
        initial = head_of(repo, "main~2")
        if history == "stale-branch-revert":
            # A branch cut before main's skill change resolves the skill back to its version.
            later = side_commit(repo, initial, "later\n", path="later.txt")
            git(repo, "merge", "-q", "--no-ff", "--no-commit", later)
            (repo / SKILL).write_text("v1\n")
            git(repo, "commit", "-q", "-am", "Merge #2")
        else:
            # A merge with an older ancestor of the base as a parent takes that ancestor's skill.
            reverted = side_commit(repo, head_of(repo), "v1\n")
            tree = head_of(repo, reverted + "^{tree}")
            merge = git(repo, "commit-tree", tree, "-p", head_of(repo), "-p", initial, "-m", "Merge #2").strip()
            git(repo, "reset", "-q", "--hard", merge)
        held = head_of(repo)
    refused = landing(repo, head_of(repo), leased)
    assert refused.returncode == 1
    assert refused.stderr.startswith("protected_instructions_changed: unreleased commits")
    assert held in refused.stderr


def test_a_malformed_release_record_fails_even_a_protected_clean_range(tmp_path, monkeypatch):
    monkeypatch.delenv("PROVENANT_ATTEMPT_MARKER", raising=False)
    repo, _ = instruction_lane(tmp_path)
    leased = head_of(repo, "main")
    (repo / "later.txt").write_text("later\n")
    git(repo, "add", "later.txt")
    git(repo, "commit", "-q", "-m", "plain change")
    assert landing(repo, head_of(repo), leased).returncode == 0
    releases = repo / ".git/provenant-instruction-checks/releases"
    releases.mkdir(parents=True, exist_ok=True)
    (releases / "bad.json").write_text('{"tip": "not-a-sha"}')
    refused = landing(repo, head_of(repo), leased)
    assert refused.returncode != 0
    assert "invalid release record bad.json" in refused.stderr


def test_a_writer_cannot_take_a_branch_another_checkout_holds(tmp_path):
    _, lane = instruction_lane(tmp_path)
    git(lane, "symbolic-ref", "HEAD", "refs/heads/main")
    with pytest.raises(ValueError, match="checked out by another worktree"):
        supervisor().build_plan("claude", {}, "hello", workspace_root=tmp_path, mode="worktree_write", worktree=lane)
    git(lane, "checkout", "-q", "--detach", "lane")
    plan = supervisor().build_plan("claude", {}, "hello", workspace_root=tmp_path, mode="worktree_write", worktree=lane)
    assert plan["lane_ref"] is None
    # Without sandbox-exec (Linux CI) there is no path boundary: writable_paths is None.
    assert not any("/refs/" in path for path in plan["applied"]["write_boundary"]["writable_paths"] or [])


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
@pytest.mark.parametrize("missing", ["packed-ref", "reflog"])
def test_a_sandboxed_writer_commits_on_a_nested_branch_without_its_ref_directories(tmp_path, missing):
    mod = supervisor()
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    repo, lane = instruction_lane(tmp_path)
    git(lane, "switch", "-q", "-c", "fix/nested")
    common = repo.resolve() / ".git"
    if missing == "packed-ref":
        git(repo, "pack-refs", "--all")
        assert not (common / "refs/heads/fix").exists()
    shutil.rmtree(common / "logs/refs/heads/fix")
    plan = mod.build_plan("claude", {}, "hello", workspace_root=tmp_path, mode="worktree_write", worktree=lane)
    assert plan["lane_ref"] == "refs/heads/fix/nested"
    profile = mod.provider_confinement.os_confinement_profile(plan)
    fixture = " ".join(shlex.quote(arg) for arg in GIT_FIXTURE)
    script = (f"echo more >> app.txt && git {fixture} commit -q -am more"
              f" && git {fixture} merge -q --no-ff --no-edit main")
    run = subprocess.run([sandbox_exec, "-p", profile, "/bin/sh", "-c", script], capture_output=True,
                         text=True, cwd=lane)
    if run.returncode and "sandbox_apply" in run.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert run.returncode == 0, run.stderr
    assert head_of(repo, "fix/nested") == head_of(lane)
    assert (common / "logs/refs/heads/fix/nested").is_file()


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_a_sandboxed_writer_moves_only_its_own_branch(monkeypatch, tmp_path):
    mod = supervisor()
    sandbox_exec = mod.provider_confinement._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    repo, lane = instruction_lane(tmp_path)
    git(repo, "update-ref", "refs/remotes/origin/main", "main")
    git(repo, "pack-refs", "--all")
    git(lane, "update-ref", "refs/heads/lane", "lane")  # A loose lane ref beside packed-refs.
    plan = mod.build_plan("claude", {}, "hello", workspace_root=tmp_path, mode="worktree_write", worktree=lane)
    profile = mod.provider_confinement.os_confinement_profile(plan)
    common = repo.resolve() / ".git"
    private = common / "worktrees/lane"

    def run(script):
        return subprocess.run([sandbox_exec, "-p", profile, "/bin/sh", "-c", script], capture_output=True,
                              text=True, cwd=lane)

    probe = run("true")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    fixture = " ".join(shlex.quote(arg) for arg in GIT_FIXTURE)
    committed = run(f"echo more >> app.txt && git {fixture} commit -q -am more")
    assert committed.returncode == 0, committed.stderr
    assert head_of(repo, "lane") == head_of(lane)
    main = head_of(repo, "main")
    for ref in ("refs/heads/main", "refs/remotes/origin/main", "refs/tags/v1", "refs/heads/other"):
        moved = run(f"git update-ref {ref} HEAD")
        assert moved.returncode != 0, ref
    assert head_of(repo, "main") == main
    for path in (common / "packed-refs", lane / ".git", private / "commondir", private / "gitdir",
                 private / "config.worktree", common / "objects/info/alternates", common / "HEAD",
                 common / "provenant-instruction-checks"):
        assert run(f"printf x >> {shlex.quote(str(path))}").returncode != 0, path


def test_lane_provenance_stamps_the_installed_harness_revision(tmp_path):
    code = """import json
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'DONE'}}), flush=True)
print(json.dumps({'type':'turn.completed'}), flush=True)
"""
    record = supervisor().execute(fixture_plan(tmp_path, code), tmp_path / "result.md")
    head = subprocess.run(["git", "-C", str(Path(supervisor().__file__).resolve().parents[3]), "rev-parse", "HEAD"],
                          capture_output=True, text=True)
    assert record["status"] == "ok"
    assert record["provenance"]["harness_rev"] == (head.stdout.strip() if head.returncode == 0 else None)


@pytest.mark.parametrize("body_string", [False, True])
@pytest.mark.parametrize("wrapped", [False, True])
def test_opencode_spent_free_allowance_records_provider_reason(tmp_path, body_string, wrapped):
    message = "Rate limit exceeded. Please try again later."
    body = {"error": {"type": "FreeUsageLimitError", "message": message}}
    error = {"type": "provider.quota", "status": 429,
             "response": {"body": json.dumps(body) if body_string else body}}
    event = {"type": "error", "error": error} if wrapped else error
    code = "import json,sys; print(json.dumps(" + repr(event) + ")); sys.exit(1)"
    record = supervisor().execute(fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md")
    assert record["status"] == "usage_limited"
    assert record["reason"] == message


def test_provider_exports_its_exact_attempt_for_nested_dispatch(tmp_path, monkeypatch):
    monkeypatch.setenv('PROVENANT_RUN_TOKEN', 'parent-run-token')
    monkeypatch.setenv('PROVENANT_PARENT_RUN_TOKEN', 'previous-lane-token')
    monkeypatch.setenv('PROVENANT_HOST_LOCAL_ONLY', '1')
    monkeypatch.setenv('PROVENANT_PARENT_REMOTE', '1')
    code = """import json,os
print(json.dumps({'type':'item.completed','item':{'type':'agent_message',
    'text':json.dumps({key: os.environ.get(key) for key in
        ('PROVENANT_PARENT_ATTEMPT','PROVENANT_PARENT_RUN_TOKEN','PROVENANT_HOST_LOCAL_ONLY','PROVENANT_PARENT_REMOTE')})}}), flush=True)
print(json.dumps({'type':'turn.completed'}), flush=True)
"""
    plan = fixture_plan(tmp_path, code)
    parent = {'run_id': 'mcp-parent', 'task_id': 'writer', 'attempt': 3, 'run_dir': str(tmp_path)}
    plan['attempt_identity'] = parent
    record = supervisor().execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'ok'
    observed = json.loads((tmp_path / 'result.md').read_text())
    assert json.loads(observed['PROVENANT_PARENT_ATTEMPT']) == parent
    assert observed['PROVENANT_PARENT_RUN_TOKEN'] == 'parent-run-token'
    assert observed['PROVENANT_HOST_LOCAL_ONLY'] is None
    assert observed['PROVENANT_PARENT_REMOTE'] is None


@pytest.mark.parametrize('mode', ['read_only', 'worktree_write'])
@pytest.mark.parametrize('explicit', [True, False, 'uncached', 'resume-default', 'default-preflight'])
@pytest.mark.parametrize('nested', [False, True, 'alias'])
def test_claude_account_binds_environment_sandbox_and_receipt(tmp_path, monkeypatch, mode, explicit, nested):
    mod = supervisor()
    home = tmp_path / 'home'
    home.mkdir()
    provider_home(monkeypatch, tmp_path, home=home, instance_root=home / '.agents',
                  state_root=tmp_path / 'state')
    monkeypatch.delenv('CLAUDE_CONFIG_DIR', raising=False)
    config = home / '.agents/config/telemetry.json'
    config.parent.mkdir(parents=True)
    selected = home / '.claude/pro' if nested is True else home / '.claude-pro'
    other = home / '.claude/max' if nested is True else home / '.claude-max'
    if nested == 'alias':
        target = tmp_path / 'identities/pro'
        target.mkdir(parents=True)
        selected.symlink_to(target, target_is_directory=True)
    else:
        selected.mkdir(parents=True)
    other.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {'claude': {
        'pro': {'identity_dir': str(selected)}, 'max': {'identity_dir': str(other), 'enabled': False},
    }}}))
    # A synthetic, identity-bound cache is the only input to automatic selection.
    import provider_quota as quota
    quota.save_snapshot({'schema': quota.SCHEMA, 'host': quota.telemetry_host(), 'provider': 'claude',
                         'account': 'pro', 'status': 'ok', 'windows': [
                             {'name': 'five_hour', 'used_percent': 10, 'window_minutes': 300},
                             {'name': 'seven_day', 'used_percent': 10, 'window_minutes': 10080},
                         ]}, selected)
    if explicit in ('uncached', 'resume-default', 'default-preflight'):
        quota.cache_path('claude', 'pro', identity_dir=selected).unlink()
        if explicit != 'uncached':
            selected = home / '.claude'
    code = "import os,json; print(json.dumps({'type':'result','is_error':False,'result':str(os.environ.get('CLAUDE_CONFIG_DIR'))}))"
    plan = fixture_plan(tmp_path, code, 'claude', mode=mode, account='pro' if explicit is True else 'default' if explicit in ('resume-default', 'default-preflight') else None,
                        resume_session='01234567-0123-4567-89ab-0123456789ab' if explicit == 'resume-default' else None)
    profile = mod.provider_confinement.os_confinement_profile(plan)
    assert f'(allow file-read-data (subpath "{selected.resolve()}")' in profile
    assert f'(subpath "{other}")' in profile
    assert any(line.startswith('(deny file-read') and f'"{other}"' in line for line in profile.splitlines())
    assert not any(line.startswith('(allow ') and f'"{other}"' in line for line in profile.splitlines())
    if explicit not in ('resume-default', 'default-preflight'):
        assert not any(line.startswith('(allow ') and f'"{home / ".claude"}"' in line for line in profile.splitlines())
    record = mod.execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'ok'
    assert (tmp_path / 'result.md').read_text().strip() == (
        'None' if explicit in ('resume-default', 'default-preflight') else str(selected))
    expected = 'default' if explicit in ('resume-default', 'default-preflight') else 'pro'
    assert record['account'] == expected
    assert record['provenance']['account'] == expected


@pytest.mark.parametrize('inherited', [None, '', '~/.claude', 'custom'])
def test_claude_ambient_account_preserves_inherited_config_environment(tmp_path, monkeypatch, inherited):
    home = tmp_path / 'home'
    home.mkdir()
    provider_home(monkeypatch, tmp_path, home=home, instance_root=home / '.agents',
                  state_root=tmp_path / 'state')
    if inherited is None:
        monkeypatch.delenv('CLAUDE_CONFIG_DIR', raising=False)
    else:
        monkeypatch.setenv('CLAUDE_CONFIG_DIR', inherited)
    code = ("import os,json; print(json.dumps({'type':'result','is_error':False,'result':"
            "json.dumps({'set':'CLAUDE_CONFIG_DIR' in os.environ,'value':os.environ.get('CLAUDE_CONFIG_DIR')})}))")
    plan = fixture_plan(tmp_path, code, 'claude')
    record = supervisor().execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'ok'
    assert json.loads((tmp_path / 'result.md').read_text()) == {'set': inherited is not None, 'value': inherited}


@pytest.mark.parametrize('accounts', [['broken'], {'claude': []}, {'claude': {'INVALID': {}}}])
@pytest.mark.parametrize('inherited', [False, True])
def test_claude_malformed_accounts_warn_and_launch_ambient_identity(tmp_path, monkeypatch, accounts, inherited):
    home = tmp_path / 'home'
    home.mkdir()
    provider_home(monkeypatch, tmp_path, home=home, instance_root=home / '.agents',
                  state_root=tmp_path / 'state')
    if inherited:
        monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(home / '.claude-work'))
    else:
        monkeypatch.delenv('CLAUDE_CONFIG_DIR', raising=False)
    config = home / '.agents/config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': accounts}))
    code = "import os,json; print(json.dumps({'type':'result','is_error':False,'result':str(os.environ.get('CLAUDE_CONFIG_DIR'))}))"
    plan = fixture_plan(tmp_path, code, 'claude')
    assert any('invalid Claude account configuration' in warning for warning in plan['warnings'])
    assert plan['provider_home'] == str(home / ('.claude-work' if inherited else '.claude'))
    record = supervisor().execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'ok'
    assert (tmp_path / 'result.md').read_text().strip() == (str(home / '.claude-work') if inherited else 'None')


@pytest.mark.parametrize('accounts', [['broken'], {'claude': []}, {'claude': {'INVALID': {}}}])
@pytest.mark.parametrize('account', ['pro', 'default'])
def test_claude_malformed_accounts_refuse_an_explicit_account(tmp_path, monkeypatch, accounts, account):
    home = tmp_path / 'home'
    home.mkdir()
    provider_home(monkeypatch, tmp_path, home=home, instance_root=home / '.agents',
                  state_root=tmp_path / 'state')
    config = home / '.agents/config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': accounts}))
    with pytest.raises(ValueError, match='invalid Claude account configuration.*explicit --account'):
        fixture_plan(tmp_path, "raise AssertionError('must never launch')", 'claude', account=account)


@pytest.mark.parametrize('mode', ['read_only', 'worktree_write'])
@pytest.mark.parametrize('selected', ['.claude-pro', '.claude/pro', '.claude'])
def test_claude_config_read_grants_follow_selected_identity(tmp_path, monkeypatch, mode, selected):
    home = tmp_path / 'home'
    home.mkdir()
    monkeypatch.setenv('HOME', str(home))
    plan = {'adapter': 'claude', 'mode': mode, 'cwd': str(tmp_path), 'workspace_root': str(tmp_path),
            'provider_home': str(home / selected), 'applied': {'add_dirs': []}}
    profile = supervisor().provider_confinement.os_confinement_profile(plan)
    grants = '\n'.join(line for line in profile.splitlines() if line.startswith('(allow file-read'))
    assert f'(subpath "{home / selected}")' in grants
    assert (f'"{home / ".claude.json"}"' in grants) is (selected == '.claude' and mode == 'read_only')
    denied = '\n'.join(line for line in profile.splitlines() if line.startswith('(deny file-read'))
    assert (f'"{home / ".claude.json"}"' in denied) is (selected != '.claude')


def test_claude_all_exhausted_refuses_plan_before_launch(tmp_path, monkeypatch):
    home = tmp_path / 'home'
    home.mkdir()
    provider_home(monkeypatch, tmp_path, home=home, instance_root=home / '.agents',
                  state_root=tmp_path / 'state')
    monkeypatch.delenv('CLAUDE_CONFIG_DIR', raising=False)
    config = home / '.agents/config/telemetry.json'
    config.parent.mkdir(parents=True)
    identities = {label: home / ('.claude-' + label) for label in ('pro', 'max')}
    config.write_text(json.dumps({'accounts': {'claude': {
        label: {'identity_dir': str(path)} for label, path in identities.items()}}}))
    import provider_quota as quota
    for label, path in identities.items():
        quota.save_snapshot({'schema': quota.SCHEMA, 'host': quota.telemetry_host(),
                             'provider': 'claude', 'account': label, 'status': 'ok',
                             'windows': [{'name': 'five_hour', 'used_percent': 100,
                                          'reset_at': (datetime.now(UTC) + timedelta(hours=2)).isoformat()},
                                         {'name': 'seven_day', 'used_percent': 10}]}, path)
    with pytest.raises(ValueError, match='claude_accounts_exhausted'):
        fixture_plan(tmp_path, "raise AssertionError('must never launch')", 'claude')


def test_claude_switch_from_custom_ambient_to_default_unsets_config_dir(tmp_path, monkeypatch):
    home = tmp_path / 'home'
    home.mkdir()
    provider_home(monkeypatch, tmp_path, home=home, instance_root=home / '.agents',
                  state_root=tmp_path / 'state')
    monkeypatch.setenv('CLAUDE_CONFIG_DIR', str(home / '.claude-work'))
    config = home / '.agents/config/telemetry.json'
    config.parent.mkdir(parents=True)
    config.write_text(json.dumps({'accounts': {'claude': {'default': {'identity_dir': str(home / '.claude')}}}}))
    code = ("import os,json; print(json.dumps({'type':'result','is_error':False,'result':"
            "str('CLAUDE_CONFIG_DIR' in os.environ)}))")
    plan = fixture_plan(tmp_path, code, 'claude', account='default')
    record = supervisor().execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'ok'
    assert (tmp_path / 'result.md').read_text().strip() == 'False'


@pytest.mark.parametrize('cleanup_error', ['agy_home', 'browser_lstat'])
def test_cancelled_attempt_stops_nested_children_even_when_cleanup_fails(tmp_path, monkeypatch, cleanup_error):
    mod = supervisor()
    parent_dir = tmp_path / '.agent-run/runs/parent'
    child_dir = parent_dir.parent / 'child'
    journal = parent_dir / 'dispatch/tasks/writer/attempt-001/tmp/children'
    journal.mkdir(parents=True)
    child_dir.mkdir()
    parent = {'run_id': 'mcp-parent', 'run_dir': str(parent_dir), 'task_id': 'writer', 'attempt': 1}
    (child_dir / 'RUN_RECEIPT.json').write_text(json.dumps({'status': 'active', 'parent': parent}))
    child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                             env={**os.environ, 'PROVENANT_RUN_TOKEN': 'child-token',
                                  'PROVENANT_RUN_DIR': str(child_dir)},
                             start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        (child_dir / 'dispatch-owner.json').write_text(json.dumps({
            'schema_version': 1, 'kind': 'dispatch', 'run_dir': str(child_dir),
            'run_token': 'child-token', 'owner_pid': child.pid, 'owner_pgid': child.pid,
            'owner_started_at': mod.process_info.start_time(child.pid),
        }))
        (journal / 'child.json').write_text(json.dumps({
            'run_id': 'mcp-child', 'run_dir': str(child_dir), 'run_token': 'child-token'}))
        plan = fixture_plan(tmp_path, 'import time; time.sleep(30)',
                            adapter='agy' if cleanup_error == 'agy_home' else 'claude',
                            capabilities=[] if cleanup_error == 'agy_home' else ['browser'],
                            run_dir=tmp_path / 'attempt')
        plan['attempt_identity'] = parent
        if cleanup_error == 'agy_home':
            agy_home_fixture(mod, monkeypatch, tmp_path)
            plan['applied']['confinement'] = 'sandbox-exec'
            # Run the fixture process directly while exercising real home staging and cleanup.
            monkeypatch.setattr(mod.provider_confinement, 'confinement_command', lambda plan, command, search_path=None: command)
            finish = mod.agy_home.finish_agy_home

            def failed_finish(*args):
                finish(*args)
                raise RuntimeError('home cleanup failed')

            monkeypatch.setattr(mod.agy_home, 'finish_agy_home', failed_finish)
            with pytest.raises(RuntimeError, match='home cleanup failed'):
                mod.execute(plan, tmp_path / 'result.md', cancelled=lambda: True)
        else:
            temporary = Path(plan['applied']['browser_tmpdir'])
            lstat = Path.lstat
            started = False

            def unreadable_tmp(path, *args, **kwargs):
                if started and path == temporary:
                    raise PermissionError('browser temp inspection denied')
                return lstat(path, *args, **kwargs)

            def on_start(process):
                nonlocal started
                started = True

            monkeypatch.setattr(Path, 'lstat', unreadable_tmp)
            try:
                record = mod.execute(plan, tmp_path / 'result.md', cancelled=lambda: started, on_start=on_start)
                assert record['status'] == 'cancelled'
                assert any('browser temp inspection denied' in warning for warning in record['warnings'])
            finally:
                started = False
                if temporary.exists():
                    shutil.rmtree(temporary)
        assert child.wait(timeout=2) == -signal.SIGTERM
    finally:
        if child.poll() is None:
            child.kill()
        child.wait(timeout=5)


@pytest.mark.parametrize("model,effort,listing_status,listing,expected", [
    ("gemini-valid", "high", 0, "gemini-valid", "passed"),
    ("gemini-vlaid", "high", 0, "gemini-valid", "invalid"),
    ("gemini-valid", "high", 1, "gemini-valid", "unavailable"),
    ("gemini-valid", "high", 0, "", "unavailable"),
    ("GEMINI-3.8-FLASH", "low", 0, "gemini-3.8-flash-low", "passed"),
    ("gemini-valid", "high", 0, "Usage: agy models\nfallback", "unavailable"),
])
def test_dispatch_checks_live_adapter_listing(tmp_path, monkeypatch, model, effort, listing_status, listing, expected):
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(tmp_path / "state"))
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    launched = tmp_path / "launched"
    adapter = tmp_path / "agy-stub"
    adapter.write_text(
        f"#!{sys.executable}\n"
        "import json, pathlib, sys\n"
        "if sys.argv[1:] == ['--version']: print('stub 1.0')\n"
        "elif sys.argv[1:] == ['--help']: print('--model')\n"
        "elif sys.argv[1:] == ['models']:\n"
        f"    print({listing!r}); sys.exit({listing_status})\n"
        "else:\n"
        f"    pathlib.Path({str(launched)!r}).touch()\n"
        "    print(json.dumps({'event':'result', 'result':{'status':'SUCCESS', 'response':'DONE'}}))\n"
    )
    adapter.chmod(0o755)
    state = tmp_path / "state"
    state.mkdir()
    (state / "capabilities.json").write_text(json.dumps({"agy": {
        "version": "stub 1.0", "executable": str(adapter.resolve()),
        "observed_at": datetime.now(UTC).isoformat(), "models": ["gemini-old"],
    }}))
    plan = fixture_plan(tmp_path, "", "agy", requested_model=model)
    plan["model"] = model
    plan["effort"] = effort
    plan["argv"] = [str(adapter), "--model", model]
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["model_check"] == expected
    if expected == "unavailable":
        assert json.loads((state / "capabilities.json").read_text())["agy"]["models"] == ["gemini-old"]
    if expected == "invalid":
        assert record["status"] == "model_unavailable"
        assert record["model_check"] == "invalid"
        assert "gemini-vlaid" in record["model_check_detail"]
        assert "gemini-valid" in record["model_check_detail"]
        assert "gemini-vlaid" in record["fix"] and "gemini-valid" in record["fix"]
        assert not launched.exists()
    else:
        assert record["status"] == "ok"
        assert launched.exists()


def test_receipt_keeps_model_changes_when_provider_switches_back(tmp_path):
    models = ["gemini-valid", "gemini-cheap", "gemini-valid"]
    events = [{"type": "assistant", "message": {"model": model, "content": []}}
              for model in models]
    events.append({"type": "result", "result": "DONE", "is_error": False})
    plan = fixture_plan(tmp_path, f"import json; events={events!r}; [print(json.dumps(e)) for e in events]", "claude")
    plan["model"] = models[0]
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["provenance"]["observed_model"] == models[0]
    assert record["model_changes"] == [
        {"from": models[0], "to": models[1]},
        {"from": models[1], "to": models[2]},
    ]
    assert not record["certification_eligible"]
    assert "model changed during execution" in "\n".join(record["warnings"])


def test_claude_subagent_model_does_not_change_parent_route(tmp_path):
    events = [
        {"type": "assistant", "message": {"model": "claude-sonnet-5-5", "content": []}},
        {"type": "assistant", "parent_tool_use_id": "tool-1",
         "message": {"model": "claude-haiku-4-5", "content": []}},
        {"type": "result", "result": "DONE", "is_error": False},
    ]
    plan = fixture_plan(tmp_path, f"import json; events={events!r}; [print(json.dumps(e)) for e in events]", "claude")
    plan["model"] = "claude-sonnet-5-5"
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["provenance"]["observed_model"] == plan["model"]
    assert record["model_changes"] == []
    assert record["model_check"] == "unsupported"
    assert not any("model list" in warning.lower() for warning in record["warnings"])


@pytest.mark.parametrize("mode", ["cache_hit", "cache_miss", "untrusted_cache", "cancel_during_probe", "cancel_before_probe", "auto"])
def test_dispatch_listing_cache_environment_and_cancellation(tmp_path, mode):
    listed, launched = tmp_path / "listed", tmp_path / "launched"
    cli = tmp_path / "agy"
    cli.write_text(
        f"#!{sys.executable}\nimport json, os, pathlib, sys\n"
        "assert os.environ['PROBE_ENV'] == 'attempt'\n"
        "if sys.argv[1:] == ['--version']: print('1.0')\n"
        "elif sys.argv[1:] == ['--help']: print('--model')\n"
        "elif sys.argv[1:] == ['models']:\n"
        f"    with open({str(listed)!r}, 'a') as calls: calls.write('listed\\n')\n"
        "    print('gemini-3.8-flash-low')\n"
        "else:\n"
        f"    pathlib.Path({str(launched)!r}).touch()\n"
        "    print(json.dumps({'event': 'result', 'result': {'status': 'SUCCESS', 'response': 'DONE'}}))\n"
    )
    cli.chmod(0o755)
    cache = Path(os.environ["AGENT_FABRIC_STATE_ROOT"]) / "capabilities.json"
    cache.write_text(json.dumps({"agy": {
        "version": "1.0", "executable": str(cli.resolve()),
        "observed_at": datetime.now(UTC).isoformat(),
        "models": ["gemini-3.8-flash-low" if mode in {"cache_hit", "untrusted_cache"} else "gemini-old"],
        **({"listing_format": 1} if mode != "untrusted_cache" else {}),
    }}))
    plan = fixture_plan(tmp_path, "", "agy")
    plan["model"] = "auto" if mode == "auto" else "gemini-3.8-flash"
    plan["effort"] = "low"
    plan["argv"] = [str(cli), "--model", plan["model"]]
    cancelled = lambda: mode == "cancel_before_probe" or (mode == "cancel_during_probe" and listed.exists())
    record = supervisor().execute(plan, tmp_path / "result.md", cancelled=cancelled,
                                  env={**os.environ, "PROBE_ENV": "attempt"})
    assert (listed.read_text().splitlines() if listed.exists() else []) == (
        ["listed"] if mode in {"cache_miss", "untrusted_cache", "cancel_during_probe"} else [])
    cancelled = mode.startswith("cancel_")
    assert record["status"] == ("cancelled" if cancelled else "ok")
    assert launched.exists() is not cancelled
    assert record["model_check"] == ("skipped" if mode in {"auto", "cancel_before_probe"} else "passed")
    assert not any("model list" in warning.lower() for warning in record["warnings"])


def test_agy_stderr_fallback_after_init_is_a_pinned_substitution(tmp_path):
    code = """import json, sys, time
print(json.dumps({'event': 'init', 'init': {'model': 'gemini-valid'}}), flush=True)
time.sleep(0.1)
sys.stderr.write('using mo'); sys.stderr.flush()
time.sleep(0.1)
sys.stderr.write('del gemini-cheap'); sys.stderr.flush()
print(json.dumps({'event': 'result', 'result': {'status': 'SUCCESS', 'response': 'DONE'}}), flush=True)
"""
    plan = fixture_plan(tmp_path, code, "agy", requested_model="gemini-valid")
    plan["model"] = "gemini-valid"
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "model_unavailable"
    assert record["evidence"]["signature"] == "pinned_model_substitution"
    assert record["provenance"]["observed_model"] == "gemini-cheap"
    assert record["model_changes"] == [{"from": "gemini-valid", "to": "gemini-cheap"}]
    assert not record["certification_eligible"]


@pytest.mark.parametrize("diagnostic", [
    'DEBUG request model=gemini-cheap',
    'INFO model: gemini-cheap',
    'notusing model gemini-cheap',
])
def test_agy_log_model_fields_do_not_substitute_pinned_route(tmp_path, diagnostic):
    code = (f"import json, sys; print({diagnostic!r}, file=sys.stderr); "
            "print(json.dumps({'event': 'result', 'result': {'status': 'SUCCESS', 'response': 'DONE'}}))")
    plan = fixture_plan(tmp_path, code, "agy", requested_model="gemini-valid")
    plan["model"] = "gemini-valid"
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["model_changes"] == []


@pytest.mark.parametrize("synthetic_last", [False, True])
@pytest.mark.parametrize("synthetic_kind", ["assistant", "init"])
def test_claude_synthetic_model_does_not_change_parent_route(tmp_path, synthetic_last, synthetic_kind):
    models = ["<synthetic>", "claude-sonnet-5-5"]
    if synthetic_last:
        models.reverse()
    events = [{"type": "system", "subtype": "init", "model": model}
              if model == "<synthetic>" and synthetic_kind == "init" else
              {"type": "assistant", "message": {"model": model, "content": []}} for model in models]
    events.append({"type": "result", "result": "DONE", "is_error": False})
    plan = fixture_plan(tmp_path, f"import json; events={events!r}; [print(json.dumps(e)) for e in events]", "claude")
    plan["model"] = "claude-sonnet-5-5"
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert record["provenance"]["observed_model"] == plan["model"]
    assert record["model_changes"] == []
    assert not any("mismatch" in warning or "model changed" in warning for warning in record["warnings"])
    assert "<synthetic>" not in json.dumps(record["provenance"])
