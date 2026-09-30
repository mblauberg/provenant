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
import subprocess
import ctypes
import sys
import tempfile
import time
import tomllib
from types import SimpleNamespace

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "skills/orchestrate/scripts"


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
            str(SCRIPTS / "cf_dispatch.sh"),
            "--intent",
            "ordinary",
            "--tool",
            "claude",
            "--alias",
            "workhorse",
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
    monkeypatch.setattr(supervisor, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    profile = supervisor.os_confinement_profile(plan)
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
    monkeypatch.setattr(supervisor, "os_confinement_profile", lambda _plan, _search_path=None: "(version 1)")
    assert supervisor.confinement_command(plan, ["/bin/cat", "file"]) == [
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
    profile = supervisor.os_confinement_profile(plan)

    assert f'(literal "{home / ".gitconfig"}")' in profile
    assert f'(literal "{xdg / "git/config"}")' in profile
    assert f'(subpath "{home}")' in profile
    assert f'(literal "{home / ".ssh/config"}")' not in profile

    monkeypatch.delenv("XDG_CONFIG_HOME")
    default_profile = supervisor.os_confinement_profile(plan)
    assert f'(literal "{home / ".config/git/config"}")' in default_profile


@pytest.mark.parametrize("adapter", ["agy", "claude", "cursor", "opencode", "kiro"])
def test_writer_confinement_allows_owned_paths_and_add_dirs(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    worktree = tmp_path / "worktree"
    attempt = tmp_path / "run" / "attempt"
    common = tmp_path / "common.git"
    private = tmp_path / "linked.git"
    add_dir = tmp_path / "extra"
    for path in (worktree, attempt, common, private, add_dir):
        path.mkdir(parents=True)
    dirs = iter((private, common))
    monkeypatch.setattr(mod.subprocess, "run", lambda *a, **k: SimpleNamespace(returncode=0, stdout=str(next(dirs))))
    plan = {"adapter": adapter, "mode": "worktree_write", "cwd": str(worktree),
            "workspace_root": str(worktree), "run_dir": str(attempt),
            "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]}}
    profile = mod.os_confinement_profile(plan)
    assert "(deny file-write*)" in profile
    assert f'(deny file-write* (subpath "{common}"))' in profile
    allow = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-write* "))
    for path in (worktree, add_dir, attempt, private, Path("/dev")):
        assert f'(subpath "{path}")' in allow
    assert f'(subpath "{common}")' not in allow
    for path in (common / "objects", common / "refs", common / "logs"):
        assert f'(subpath "{path}")' in profile
    for path in (common / "packed-refs", common / "packed-refs.lock", common / "packed-refs.new"):
        assert f'(literal "{path}")' in profile
    for path in (common / "config", common / "hooks", common / "HEAD", common / "index"):
        assert f'(subpath "{path}")' not in profile
        assert f'(literal "{path}")' not in profile
    for path in (Path.home() / ".cache", Path.home() / "Library/Caches", Path("/private/tmp")):
        assert f'(subpath "{path}")' not in profile
    for state_path in mod.CONFINED_STATE[adapter]["read_write"]:
        assert mod._sbpl_filter(Path.home() / state_path) in profile
    assert f'(subpath "{tmp_path}")' not in profile
    assert '(subpath "/dev")' in profile
    assert "(deny file-read-data" not in profile


def test_writer_protected_path_inside_add_dir_keeps_read_and_write_denied(monkeypatch, tmp_path):
    mod = supervisor()
    worktree = tmp_path / "worktree"
    add_dir = tmp_path / "extra"
    protected = add_dir / "private"
    worktree.mkdir()
    protected.mkdir(parents=True)
    monkeypatch.setattr(mod.subprocess, "run", lambda *a, **k: SimpleNamespace(returncode=1, stdout=""))
    plan = {"adapter": "agy", "mode": "worktree_write", "cwd": str(worktree),
            "workspace_root": str(worktree), "protected_paths": [str(protected)],
            "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]}}

    profile = mod.os_confinement_profile(plan)

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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    claude = mod.build_plan("claude", {}, "hello", cwd=tmp_path, mode="worktree_write")
    codex = mod.build_plan("codex", {}, "hello", cwd=tmp_path, mode="worktree_write")
    assert claude["applied"]["confinement"] == "sandbox-exec"
    assert codex["applied"]["confinement"] == "provider-native"
    name, overrides = codex_profile(codex)
    assert f'permissions.{name}.extends=":workspace"' in overrides
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: None)
    degraded = mod.build_plan("claude", {}, "hello", cwd=tmp_path, mode="worktree_write")
    assert degraded["applied"]["confinement"] == "none"
    assert any("worktree_write writes are unconfined" in item for item in degraded["warnings"])


def test_agy_writer_requires_os_confinement_and_uses_write_profile(monkeypatch, tmp_path):
    mod = supervisor()
    add_dir = tmp_path.parent / "agy-extra"
    add_dir.mkdir()
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: None)
    with pytest.raises(ValueError, match="sandbox-exec"):
        mod.build_plan("agy", {}, "hello", cwd=tmp_path, mode="worktree_write")

    (tmp_path / ".agents").mkdir()
    (tmp_path / ".agents/fabric-policy.json").write_text(
        '{"protected_paths":["secret/"]}'
    )
    probes = iter(("/usr/bin/sandbox-exec", None))
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: next(probes))
    with pytest.raises(ValueError, match="sandbox-exec"):
        mod.build_plan("agy", {}, "hello", cwd=tmp_path, mode="worktree_write")

    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    plan = mod.build_plan("agy", {}, "hello", cwd=tmp_path, mode="worktree_write",
                          add_dirs=[add_dir])
    assert plan["applied"]["confinement"] == "sandbox-exec"
    assert "--dangerously-skip-permissions" in plan["argv"]
    assert "--sandbox" not in plan["argv"]
    assert plan["protected_paths"] == [str(tmp_path / "secret")]
    profile = mod.os_confinement_profile(plan)
    assert "(deny file-write*)" in profile
    assert f'(allow file-write* (subpath "{tmp_path}")' in profile
    assert f'(subpath "{add_dir}")' in "\n".join(
        line for line in profile.splitlines() if line.startswith("(allow file-write* ")
    )
    assert str(add_dir) in plan["applied"]["write_boundary"]["writable_paths"]
    assert profile.rstrip().endswith(
        f'(deny file-read* (subpath "{tmp_path / "secret"}"))'
    )
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: None)
    with pytest.raises(RuntimeError, match="sandbox-exec"):
        mod.confinement_command(plan, plan["argv"])


@pytest.mark.parametrize("adapter", ["agy", "opencode", "claude", "cursor", "kiro"])
def test_wrapped_writer_uses_only_provider_cache(monkeypatch, tmp_path, adapter):
    mod = supervisor()
    profile = mod.os_confinement_profile({
        "adapter": adapter, "mode": "worktree_write", "cwd": str(tmp_path),
        "workspace_root": str(tmp_path), "run_dir": str(tmp_path / "run"), "applied": {},
    })
    for path in (Path.home() / ".cache", Path.home() / "Library/Caches", Path("/private/tmp")):
        assert f'(subpath "{path}")' not in profile
    for path in mod.CONFINED_STATE[adapter]["read_write"]:
        assert mod._sbpl_filter(Path.home() / path) in profile


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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
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
    assert mod.protected_paths(tmp_path) == []


def test_writer_discovers_worktree_policy_outside_workspace(monkeypatch, tmp_path):
    mod = supervisor()
    repo = protected_repo(tmp_path)
    workspace = tmp_path / "workspace"
    workspace.mkdir()
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
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
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c",
                    "user.email=test@example.invalid", "commit", "-q", "--allow-empty", "-m", "initial"], check=True)
    linked = repo / ".worktrees/linked"
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "-q", "-b", "linked", str(linked)], check=True)
    (repo / ".agents").mkdir()
    (repo / ".agents/fabric-policy.json").write_text('{"protected_paths":["secret/"]}')
    for root in (repo, linked):
        (root / "secret").mkdir()
        (root / "secret/prompt.md").write_text("sensitive")
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    plan = mod.build_plan("opencode", {"trains_on_prompts": True}, "hello",
                          cwd=workspace, workspace_root=workspace)
    profile = mod.os_confinement_profile(plan)
    deny = next(line for line in profile.splitlines() if line.startswith("(deny file-read* "))
    for root in (repo, linked):
        assert f'(subpath "{root / "secret"}")' in deny
    monkeypatch.chdir(workspace)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(ROOT))
    for root in (repo, linked):
        task = {"id": "one", "adapter": "opencode", "model": "opencode/mimo-v2.6-flash-free",
                "prompt_file": str(root / "secret/prompt.md"), "cwd": str(workspace)}
        result = importlib.import_module("skills.orchestrate.scripts.dispatch_run").preflight_tasks([task])
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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    plan = mod.build_plan("opencode", {"trains_on_prompts": True}, "hello",
                          cwd=repo / "safe", workspace_root=repo)
    profile = mod.os_confinement_profile(plan)
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
    result = importlib.import_module("skills.orchestrate.scripts.dispatch_run").preflight_tasks([task])
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
    result = importlib.import_module("skills.orchestrate.scripts.dispatch_run").preflight_tasks([task], repo)
    assert result["status"] == "rejected"
    assert "protected path" in result["fix"]


def test_unresolved_training_flag_fails_closed_without_sandbox(monkeypatch, tmp_path):
    mod = supervisor()
    repo = protected_repo(tmp_path)
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: None)
    with pytest.raises(ValueError, match="sandbox-exec"):
        mod.build_plan("claude", {}, "hello", cwd=repo / "safe", workspace_root=repo)


def test_protected_preflight_rejects_unavailable_sandbox(monkeypatch, tmp_path):
    repo = protected_repo(tmp_path)
    monkeypatch.chdir(repo)
    monkeypatch.setenv("AGENT_FABRIC_PRODUCT_ROOT", str(ROOT))
    monkeypatch.setenv("AGENT_FABRIC_INSTANCE_ROOT", str(ROOT))
    dispatch_run = importlib.import_module("skills.orchestrate.scripts.dispatch_run")
    # dispatch_run imports provider_exec by its script name; patch that module object.
    monkeypatch.setattr(dispatch_run.provider_exec, "_sandbox_exec_path", lambda: None)
    task = {"id": "one", "adapter": "opencode", "model": "opencode/mimo-v2.6-flash-free",
            "prompt": "hello", "cwd": str(repo / "safe")}
    result = dispatch_run.preflight_tasks([task])
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
    subprocess.run(["git", "init", "-q", str(repo)], check=True)
    subprocess.run(["git", "-C", str(repo), "-c", "user.name=Test", "-c",
                    "user.email=test@example.invalid", "commit", "-q", "--allow-empty", "-m", "initial"], check=True)
    caller = tmp_path / "caller"
    sibling = tmp_path / "sibling"
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "-q", "-b", "caller", str(caller)], check=True)
    subprocess.run(["git", "-C", str(repo), "worktree", "add", "-q", "-b", "sibling", str(sibling)], check=True)

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


def test_read_only_profile_denies_writes_outside_attempt_and_provider_state(monkeypatch, tmp_path):
    mod = supervisor()
    home = tmp_path / "home"
    workspace = home / "repo"
    attempt = workspace / ".agent-run/attempt"
    add_dir = tmp_path / "extra"
    attempt.mkdir(parents=True)
    add_dir.mkdir()
    monkeypatch.setattr(mod.Path, "home", lambda: home)
    monkeypatch.setattr(mod, "CONFINED_STATE", {"agy": {"read_write": ("state",), "read": ()}})
    plan = {"adapter": "agy", "mode": "read_only", "workspace_root": str(workspace),
            "cwd": str(workspace), "run_dir": str(attempt),
            "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]}}
    profile = mod.os_confinement_profile(plan)
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
    profile = mod.os_confinement_profile(plan)
    allow = "\n".join(line for line in profile.splitlines() if line.startswith("(allow file-read-data "))
    # An OAuth lane reads its sign-in from the login keychain; a bare or endpoint lane uses a key.
    assert (f'"{home / "Library/Keychains/login.keychain-db"}"' in allow) is granted
    assert f'(subpath "{home / "Library/Keychains"}")' not in allow


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
    profile = mod.os_confinement_profile(plan)
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
    sandbox_exec = mod._sandbox_exec_path()
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
    for adapter, state in importlib.import_module("skills.orchestrate.scripts.provider_exec").CONFINED_STATE.items()
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
    assert f'"{home / entry}"' in mod.os_confinement_profile(plan)
    planted = home / entry
    planted.parent.mkdir(parents=True, exist_ok=True)
    if planted.exists():
        planted.unlink()
    planted.symlink_to(target)
    # An earlier lane planted a link there: the next profile refuses rather than granting its target.
    with pytest.raises(PermissionError, match="provider state must not be a symlink"):
        mod.os_confinement_profile(plan)


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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    route = {"resolved_model": "auto"}
    with pytest.raises(ValueError, match="kiro-cli 9.9.9 has not installed its engine.*fix: run kiro-cli once outside"):
        mod.build_plan("kiro", route, "hello", cwd=workspace, workspace_root=workspace)
    support = home / KIRO_SUPPORT
    (support / "kas/9.9.9-abc/node_modules").mkdir(parents=True)
    (support / "node").write_text("", encoding="utf-8")
    plan = mod.build_plan("kiro", route, "hello", cwd=workspace, workspace_root=workspace)
    assert plan["applied"]["confinement"] == "sandbox-exec"
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: None)
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
    lines = mod.os_confinement_profile(plan).splitlines()
    denied = [f'(subpath "{home / path}")' for path in mod.EXTRA_DENIED_READS]
    last_deny = max(index for index, line in enumerate(lines)
                    if line.startswith("(deny file-read-data ") and all(rule in line for rule in denied))
    later = [line for line in lines[last_deny + 1:] if line.startswith("(allow file-read")]
    # Only a Claude lane's own session transcript reopens, so that it can resume.
    own = home / ".claude/projects" / re.sub(r"[^A-Za-z0-9]", "-", str(workspace.resolve()))
    session = own / plan["session_id"]
    expected = [f'(allow file-read-data (literal "{session}.jsonl") (subpath "{session}"))']
    assert later == (expected if adapter == "claude" else [])
    # Only a UUID names a session, so no id can reopen the project's memory or another directory.
    for session_id in ("memory", "..", "s-1"):
        plan["session_id"] = session_id
        assert not any(str(own) in line for line in mod.os_confinement_profile(plan).splitlines())
    del plan["session_id"]
    lines = mod.os_confinement_profile(plan).splitlines()
    assert not any(str(own) in line for line in lines)


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_claude_read_only_lane_cannot_read_other_projects_transcripts(monkeypatch, tmp_path):
    mod = supervisor()
    sandbox_exec = mod._sandbox_exec_path()
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
    profile = mod.os_confinement_profile(plan)

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
    home = Path("/Users/someone")
    projects = home / ".claude/projects"
    assert mod._claude_project_dir(home, "/Users/someone/Repos/my_app.v2") == (
        projects / "-Users-someone-Repos-my-app-v2")
    # Claude replaces each UTF-16 code unit, so a character outside the BMP becomes two dashes.
    assert mod._claude_project_dir(home, "/Users/someone/a\U0001F600b").name == "-Users-someone-a--b"
    # A long name keeps 200 characters plus Claude's 32-bit string hash in base 36; checked against
    # the directory Claude Code 2.1.285 created for this path.
    scratch = ("/private/tmp/claude-501/-Users-user-Repos-provenant/"
               "76b874fc-49eb-4d7e-9fbb-8ff3e570ed9f/scratchpad/" + "x" * 120 + "/" + "y" * 60)
    assert mod._claude_project_dir(home, scratch).name == (
        re.sub(r"[^A-Za-z0-9]", "-", scratch)[:200] + "-qpcnov")
    # Two long paths that share their first 200 characters get different, exact directories.
    first = mod._claude_project_dir(home, "/Users/someone/" + "a" * 220 + "/one")
    second = mod._claude_project_dir(home, "/Users/someone/" + "a" * 220 + "/two")
    assert first != second and first.name[:200] == second.name[:200]


def test_sbpl_filter_star_escapes_regex_without_resolving_symlink_target(tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    target = tmp_path / "target with [regex].json"
    target.write_text("secret", encoding="utf-8")
    link = tmp_path / "link with [regex].json"
    link.symlink_to(target)

    rule = supervisor._sbpl_filter(str(link) + "*")

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
    profile = supervisor.os_confinement_profile(plan)
    assert 'space\\"and\\\\slash' in profile


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_sandbox_exec_profile_denies_home_reads_and_writes_but_keeps_provider_state(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    sandbox_exec = supervisor._sandbox_exec_path()
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
    monkeypatch.setattr(supervisor, "CONFINED_STATE", {"agy": {"read_write": ("state", "creds.json*"), "read": ()}})
    plan = {
        "adapter": "agy", "mode": "read_only", "workspace_root": str(home / "repo"),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }
    profile = supervisor.os_confinement_profile(plan)

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
        f"git -C '{cwd}' config --global user.name && git -C '{cwd}' config --global user.email"
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
    sandbox_exec = supervisor._sandbox_exec_path()
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
    profile = supervisor.os_confinement_profile(plan)

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
    monkeypatch.setattr(supervisor, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    confined = supervisor.build_plan("agy", {"resolved_model": "gemini-test"}, "prompt", cwd=tmp_path, workspace_root=tmp_path)
    assert confined["applied"]["confinement"] == "sandbox-exec"
    assert confined["applied"]["guarantee"] == "best_effort"
    monkeypatch.setattr(supervisor, "_sandbox_exec_path", lambda: None)
    unconfined = supervisor.build_plan("agy", {"resolved_model": "gemini-test"}, "prompt", cwd=tmp_path, workspace_root=tmp_path)
    assert unconfined["applied"]["confinement"] == "none"
    assert unconfined["applied"]["guarantee"] == "prompt_only"
    assert any("writes are unconfined" in warning for warning in unconfined["warnings"])


@pytest.mark.parametrize("adapter", ["claude", "cursor", "kiro", "agy", "opencode"])
def test_read_only_cwd_outside_workspace_warns_when_reads_are_unconfined(
    monkeypatch, tmp_path, adapter,
):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / "workspace"
    cwd = root / "sub"
    cwd.mkdir(parents=True)
    monkeypatch.setattr(supervisor, "_sandbox_exec_path", lambda: None)

    plan = supervisor.build_plan(
        adapter, {"resolved_model": "fixture"}, "prompt", cwd=cwd, workspace_root=root,
    )

    warning = f"{adapter} read_only: cwd is not a read boundary"
    assert plan["warnings"].count(warning) == 1


def test_codex_read_only_cwd_below_workspace_does_not_warn(monkeypatch, tmp_path):
    """Codex's native read-only sandbox reads everywhere, so a cwd bounds nothing whatever it is."""
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    root = tmp_path / "workspace"
    cwd = root / "sub"
    cwd.mkdir(parents=True)
    monkeypatch.setattr(supervisor, "_sandbox_exec_path", lambda: None)

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
    monkeypatch.setattr(supervisor, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")

    plan = supervisor.build_plan(
        adapter, {"resolved_model": "fixture"}, "prompt", cwd=cwd, workspace_root=root,
    )

    assert f"{adapter} read_only: cwd is not a read boundary" not in plan["warnings"]


def test_os_confinement_opt_out_disables_sandbox_exec(monkeypatch):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    monkeypatch.setattr(supervisor.sys, "platform", "darwin")
    monkeypatch.setattr(supervisor.shutil, "which", lambda _name: "/usr/bin/sandbox-exec")
    monkeypatch.setenv("PROVENANT_NO_OS_CONFINEMENT", "1")
    assert supervisor._sandbox_exec_path() is None


def test_refused_sandbox_exec_falls_back_to_unconfined_with_warning(monkeypatch):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    monkeypatch.setattr(supervisor.sys, "platform", "darwin")
    monkeypatch.delenv("PROVENANT_NO_OS_CONFINEMENT", raising=False)
    monkeypatch.setattr(supervisor.shutil, "which", lambda _name: "/usr/bin/sandbox-exec")
    refused = subprocess.CompletedProcess([], 71, "", "sandbox-exec: sandbox_apply: Operation not permitted\n")
    monkeypatch.setattr(supervisor.subprocess, "run", lambda *_a, **_k: refused)
    supervisor._sandbox_exec_usable.cache_clear()
    try:
        assert supervisor._sandbox_exec_path() is None
    finally:
        supervisor._sandbox_exec_usable.cache_clear()


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_sandbox_exec_profile_confines_reads_to_temp_subdirectory(tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    sandbox_exec = supervisor._sandbox_exec_path()
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
    profile = supervisor.os_confinement_profile(plan)
    permitted = subprocess.run([sandbox_exec, "-p", profile, "/bin/cat", str(allowed)], capture_output=True, text=True)
    if permitted.returncode and any(word in permitted.stderr.lower() for word in ("sandbox_apply", "operation not permitted")):
        pytest.skip("sandbox_apply is refused in this test environment")
    assert permitted.returncode == 0, permitted.stderr
    assert permitted.stdout == "allowed\n"
    blocked = subprocess.run([sandbox_exec, "-p", profile, "/bin/cat", str(denied)], capture_output=True, text=True)
    assert blocked.returncode != 0


def supervisor():
    return importlib.import_module("skills.orchestrate.scripts.provider_exec")


@pytest.mark.parametrize(
    "adapter", ["claude", "codex", "opencode", "cursor", "agy", "kiro", "copilot"]
)
@pytest.mark.parametrize(
    "text,status",
    [
        ("You've hit your usage limit", "usage_limited"),
        ("Individual quota reached", "usage_limited"),
        ("rate limit exceeded; HTTP 429", "rate_limited"),
        ("Login expired; not logged in", "auth_required"),
        ("model not found", "model_unavailable"),
        ('error: invalid model selection (--model "gemini-3.8-pro" --effort "high"): --effort is not supported for model "gemini-3.8-pro"', "rejected"),
        ("invalid model selection; quota exceeded", "rejected"),
        ("permission denied", "permission_blocked"),
    ],
)
def test_failure_signatures(adapter, text, status):
    parsed = supervisor().parse_output(adapter, "", text, 1)
    assert parsed["status"] == status
    assert parsed["signature"]
    assert len(parsed["excerpt"]) <= 200


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
        result = supervisor().parse_output(
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
    parsed = supervisor().parse_output("claude", events)
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
    parsed = supervisor().parse_output("claude", "\n".join(map(json.dumps, events)))
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
    assert supervisor().parse_output("codex", events)["status"] == "ok"


def test_reset_evidence_uses_provider_time_units():
    from datetime import UTC, datetime

    at = datetime(2026, 9, 23, 1, tzinfo=UTC)
    assert (
        supervisor().parse_output(
            "claude",
            json.dumps(
                {
                    "type": "rate_limit_event",
                    "rate_limit_info": {"status": "rejected", "resetsAt": 1790157600},
                }
            ),
            "",
            1,
            at=at,
        )["reset_at"]
        == "2026-09-23T10:00:00Z"
    )
    assert (
        supervisor().parse_output(
            "agy", "", "RESOURCE_EXHAUSTED Resets in 167h", 3, at=at
        )["reset_at"]
        == "2026-09-30T00:00:00Z"
    )
    assert (
        supervisor().parse_output(
            "codex", "", "Usage limit; try again at 2:00 AM", 1, at=at
        )["reset_at"]
        == "2026-09-23T02:00:00Z"
    )


def fixture_plan(tmp_path, code, adapter="codex", **controls):
    mod = supervisor()
    previous = os.environ.get("PROVENANT_NO_OS_CONFINEMENT")
    os.environ["PROVENANT_NO_OS_CONFINEMENT"] = "1"
    try:
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
    finally:
        if previous is None:
            os.environ.pop("PROVENANT_NO_OS_CONFINEMENT", None)
        else:
            os.environ["PROVENANT_NO_OS_CONFINEMENT"] = previous
    plan["argv"] = [sys.executable, "-u", "-c", code]
    plan["grace_seconds"] = 0.1
    return plan


def test_attempt_private_temp_and_cache_environment(tmp_path):
    code = """import json, os
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':json.dumps({key: os.environ.get(key) for key in ('TMPDIR','TMP','TEMP','XDG_CACHE_HOME')})}}))
print(json.dumps({'type':'turn.completed'}))
"""
    plan = fixture_plan(tmp_path, code)
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    expected = {key: str(tmp_path / "tmp" / "cache" if key == "XDG_CACHE_HOME" else tmp_path / "tmp")
                for key in ("TMPDIR", "TMP", "TEMP", "XDG_CACHE_HOME")}
    assert (tmp_path / "tmp" / "cache").is_dir()
    assert not (tmp_path / "cache").exists()
    assert json.loads((tmp_path / "result.md").read_text()) == expected


def test_codex_capabilities_validate_the_codex_writer_envelope(monkeypatch, tmp_path):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    attempt = tmp_path / "runs/task/attempt-001"
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")

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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
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
    elif field == "sandbox_exec": monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: value)
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
    profile = mod.os_confinement_profile(plan)
    assert "(deny mach-lookup)" not in profile and "(deny ipc-sysv*)" not in profile
    plan["applied"]["confinement"] = "none"
    plan["argv"] = [sys.executable, "-u", "-c", "import json, os; print(json.dumps({'type':'result',"
                    "'result':os.environ.get('MAC_CHROMIUM_TMPDIR')}))"]
    record = mod.execute(plan, attempt / "result.md", env={**os.environ, "MAC_CHROMIUM_TMPDIR": "ambient"})
    assert record["status"] == "ok", record
    assert (attempt / "result.md").read_text() == str(attempt / "tmp")


@pytest.mark.parametrize("capabilities", [["other"], ["browser", "browser"], "browser"])
def test_codex_capabilities_reject_unknown_duplicate_and_non_list_values(monkeypatch, tmp_path, capabilities):
    mod = supervisor()
    monkeypatch.setattr(mod.sys, "platform", "darwin")
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    attempt = tmp_path / "runs/task/attempt-001"
    common = Path(git(lane, "rev-parse", "--path-format=absolute", "--git-common-dir").strip())
    plan = mod.build_plan("codex", {}, "hello", workspace_root=tmp_path, mode="worktree_write",
                          worktree=lane, sandbox="workspace-write", network=True,
                          capabilities=["postgres"], run_dir=attempt)
    profile = mod.os_confinement_profile(plan)

    assert '(deny network-outbound (remote unix-socket))' in profile
    socket_roots = [lane, *plan["applied"]["add_dirs"], attempt, plan["codex_home"]]
    expected_socket_allow = "(allow network-outbound " + " ".join(
        ['(remote unix-socket (path-literal "/private/var/run/mDNSResponder"))']
        + [f'(remote unix-socket (subpath "{Path(root).resolve()}"))' for root in socket_roots]
    ) + ")"
    assert expected_socket_allow in profile

    assert f'(deny file-write* (subpath "{common}")' in profile
    for path in (common / "objects", common / "refs", common / "logs"):
        assert f'(subpath "{path}")' in profile
        assert str(path) in plan["applied"]["write_boundary"]["writable_paths"]
    for path in (common / "packed-refs", common / "packed-refs.lock", common / "packed-refs.new"):
        assert f'(literal "{path}")' in profile
    assert f'(allow file-write* (subpath "{common}")' not in profile
    assert f'(subpath "{attempt.parent / "codex-home"}")' in profile
    assert f'(literal "{(home / "auth.json").resolve()}")' in profile
    assert f'(subpath "{home}")' not in profile
    assert '(deny mach-lookup)' in profile
    assert '(deny mach-register)' in profile
    assert '(deny signal)' in profile
    assert '(allow signal (target same-sandbox))' in profile
    assert '(deny user-preference-write)' in profile
    for service in mod.CODEX_BASE_MACH_SERVICES:
        assert f'(global-name "{service}")' in profile
    assert '(allow ipc-sysv-shm ipc-sysv-sem)' in profile
    assert 'MachPortRendezvousServer.' not in profile
    private = Path(git(lane, "rev-parse", "--path-format=absolute", "--absolute-git-dir").strip())
    assert str(private) in plan["applied"]["write_boundary"]["writable_paths"]
    assert f'(allow file-write* (subpath "{private}")' in profile
    assert f'(deny file-write* (subpath "{lane / ".git"}"))' in profile
    for name in ("config.worktree", "commondir", "gitdir"):
        assert f'(deny file-write* (literal "{private / name}"))' in profile

    for adapter in ("codex", "claude"):
        without_capabilities = mod.build_plan(
            adapter, {}, "hello", workspace_root=tmp_path, mode="worktree_write",
            worktree=lane, sandbox="workspace-write", network=True, run_dir=attempt,
        )
        other_profile = mod.os_confinement_profile(without_capabilities)
        assert "(remote unix-socket" not in other_profile
        assert f'(deny file-write* (subpath "{lane / ".git"}"))' not in other_profile
        for name in ("config.worktree", "commondir", "gitdir"):
            assert f'(deny file-write* (literal "{private / name}"))' not in other_profile

    protected = tmp_path / "protected"
    plan["protected_paths"] = [str(protected)]
    protected_profile = mod.os_confinement_profile(plan)
    assert protected_profile.rstrip().endswith(f'(deny file-read* (subpath "{protected}"))')

    browser = mod.build_plan("codex", {}, "hello", workspace_root=tmp_path, mode="worktree_write",
                             worktree=lane, sandbox="workspace-write", network=True,
                             capabilities=["browser"], run_dir=attempt)
    browser_profile = mod.os_confinement_profile(browser)
    for service in mod.CODEX_BASE_MACH_SERVICES + mod.CODEX_BROWSER_MACH_SERVICES:
        assert f'(global-name "{service}")' in browser_profile
    for prefix in mod.CODEX_BROWSER_MACH_PORT_PREFIXES:
        assert f'(global-name-prefix "{prefix}")' in browser_profile
    assert '(allow mach-register' in browser_profile
    assert 'ipc-sysv-shm' not in browser_profile


def test_codex_writer_argv_preserves_default_and_capability_sandboxes(monkeypatch, tmp_path):
    mod = supervisor()
    _, lane = instruction_lane(tmp_path)
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")

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

    for before, after in ((argv(default_linux), argv(default_darwin)),
                          (argv(resumed_linux), argv(resumed_darwin))):
        index = after.index("allow_login_shell=false")
        assert after[index - 1] == "-c"
        assert after[:index - 1] + after[index + 1:] == before

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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    _, lane = instruction_lane(tmp_path)
    home = (tmp_path / "source-codex-home").resolve()
    home.mkdir()
    (home / "auth.json").write_text("token", encoding="utf-8")
    monkeypatch.setenv("CODEX_HOME", str(home))
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
            result = subprocess.run(["/usr/bin/sandbox-exec", "-p", mod.os_confinement_profile(plan),
                                     sys.executable, "-c", script], capture_output=True, text=True)
            if result.returncode and "sandbox_apply" in result.stderr:
                pytest.skip("sandbox_apply is refused in this test environment")
            assert result.stdout.split() == [
                "ok", "PermissionError", "PermissionError", "PermissionError", "ok",
                "PermissionError", "PermissionError", "PermissionError", "ok", "PermissionError",
                "PermissionError", "PermissionError", "ok",
            ], result.stderr
            assert not (lane / "config-link").exists()
            assert (home / "auth.json").read_text(encoding="utf-8") == "refreshed"
            assert (private / "index.probe").read_text(encoding="utf-8") == "probe"


@pytest.mark.skipif(sys.platform != "darwin" or not Path("/usr/bin/sandbox-exec").exists(),
                    reason="needs macOS sandbox-exec")
def test_a_linked_git_entry_never_moves_the_writable_git_grant(monkeypatch, tmp_path):
    mod = supervisor()
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    _, lane = instruction_lane(tmp_path)
    home = (tmp_path / "source-codex-home").resolve()
    home.mkdir()
    monkeypatch.setenv("CODEX_HOME", str(home))
    common = Path(git(lane, "rev-parse", "--path-format=absolute", "--git-common-dir").strip()).resolve()
    outside = (tmp_path / "outside").resolve()
    outside.mkdir()
    shutil.rmtree(common / "logs", ignore_errors=True)
    (common / "logs").symlink_to(outside, target_is_directory=True)
    (common / "packed-refs").unlink(missing_ok=True)
    (common / "packed-refs").symlink_to(outside / "packed")
    run_dir = tmp_path / "runs/task/attempt-001"
    run_dir.mkdir(parents=True)
    plan = mod.build_plan(
        "codex", {}, "hello", workspace_root=tmp_path, mode="worktree_write",
        worktree=lane, sandbox="workspace-write", network=True,
        capabilities=["postgres"], run_dir=run_dir,
    )
    profile = mod.os_confinement_profile(plan)
    assert str(outside) not in profile
    assert f'(subpath "{common / "logs"}")' in profile
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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
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
        "MAC_CHROMIUM_TMPDIR": str(attempt / "tmp") if browser_tmp else None,
        "TMPDIR": str(attempt / "tmp"),
        "XDG_CACHE_HOME": str(attempt / "tmp/cache"),
    }
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
    resumed_profile = mod.os_confinement_profile(resumed_plan)
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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    monkeypatch.setenv("CODEX_HOME", str(source))
    plan = mod.build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", workspace_root=tmp_path,
        mode="worktree_write", worktree=lane, sandbox="workspace-write", network=True,
        capabilities=["postgres"], run_dir=attempt,
    )
    profile = mod.os_confinement_profile(plan)
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
    monkeypatch.setattr(mod, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
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


def test_opencode_free_tier_refusal_is_model_unavailable(tmp_path):
    code = "import sys; print(\"403 FreeTierError: OpenCode's free tier can only be used from within OpenCode\", file=sys.stderr); sys.exit(1)"
    record = supervisor().execute(
        fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md"
    )
    assert record["status"] == "model_unavailable"
    assert record["fix"] == (
        "OpenCode rejected the free-tier request; use a paid opencode-go model or report this"
    )


def test_opencode_free_tier_fix_uses_failure_text_beyond_excerpt_limit(tmp_path):
    preamble = "diagnostic preamble " * 20
    code = (
        "import sys; print(" + repr(preamble +
        "403 FreeTierError: OpenCode's free tier can only be used from within OpenCode") +
        ", file=sys.stderr); sys.exit(1)"
    )
    record = supervisor().execute(
        fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md"
    )

    assert record["status"] == "model_unavailable"
    assert record["fix"] == (
        "OpenCode rejected the free-tier request; use a paid opencode-go model or report this"
    )


def test_opencode_free_tier_fix_uses_structured_failure_text(tmp_path):
    event = {
        "type": "error",
        "error": "diagnostic preamble " * 20
        + "403 FreeTierError: OpenCode's free tier can only be used from within OpenCode",
    }
    code = "import json, sys; print(json.dumps(" + repr(event) + ")); sys.exit(1)"
    record = supervisor().execute(
        fixture_plan(tmp_path, code, "opencode"), tmp_path / "result.md"
    )

    assert record["status"] == "model_unavailable"
    assert record["fix"] == (
        "OpenCode rejected the free-tier request; use a paid opencode-go model or report this"
    )


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


@pytest.mark.parametrize("stop", ["cancelled", "timed_out", "normal", "stalled"])
def test_new_session_grandchild_does_not_outlive_attempt(tmp_path, stop):
    pid_path = tmp_path / "grandchild.pid"
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
    print(json.dumps({{'type': 'item.completed', 'item': {{'type': 'agent_message', 'text': 'DONE'}}}}), flush=True)
    print(json.dumps({{'type': 'turn.completed'}}), flush=True)
else:
    time.sleep(60)
"""
    plan = fixture_plan(
        tmp_path,
        code,
        timeout_seconds=3 if stop == "timed_out" else 8,
        idle_seconds=1.6 if stop == "stalled" else 8,
    )
    if stop == "normal":
        plan["warnings"].append("route warning " + "x" * 240)
    started = time.monotonic()
    unrelated = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(60)"],
        start_new_session=True,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        def wait_for_pid_file(_process):
            if stop != "timed_out":
                return
            deadline = time.monotonic() + 5
            while not pid_path.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            assert pid_path.exists(), "provider did not publish its child PID"

        record = supervisor().execute(
            plan,
            tmp_path / "result.md",
            on_start=wait_for_pid_file,
            cancelled=lambda: stop == "cancelled"
            and pid_path.exists()
            and time.monotonic() - started >= 1.5,
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


def test_fast_double_fork_is_reaped_after_provider_exit(tmp_path):
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
        assert not _live_process(pid)
        assert any(row["pid"] == pid for row in record["reaped"])
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


def test_watchdog_census_runs_about_once_per_second(tmp_path, monkeypatch):
    module = supervisor()
    counts = []
    stopping = []
    original_sample = module._Descendants.sample
    original_stop = module._Descendants.stop

    def counted(self, *args, **kwargs):
        if not stopping:
            counts.append(time.monotonic())
        return original_sample(self, *args, **kwargs)

    def stopped(self, *args, **kwargs):
        stopping.append(True)
        return original_stop(self, *args, **kwargs)

    monkeypatch.setattr(module._Descendants, "sample", counted)
    monkeypatch.setattr(module._Descendants, "stop", stopped)
    plan = fixture_plan(tmp_path, "import time; time.sleep(1.35)", timeout_seconds=5)
    module.execute(plan, tmp_path / "result.md")
    assert 1 <= len(counts) <= 3


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
    plan = fixture_plan(tmp_path, code, timeout_seconds=8, idle_seconds=8)
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
def test_forged_fabric_token_does_not_spare_command(tmp_path, terminal_event):
    pid_path = tmp_path / "forged.pid"
    code = f"""import json,os,pathlib,subprocess,time
environment = dict(os.environ)
environment.update(PROVENANT_RUN_TOKEN='x', PROVENANT_RUN_DIR={str(tmp_path / 'not-a-run')!r})
child = subprocess.Popen(['sleep', '60'], start_new_session=True, env=environment,
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path({str(pid_path)!r}).write_text(str(child.pid))
if {terminal_event!r}:
    print(json.dumps({{'type':'item.completed','item':{{'type':'agent_message','text':'DONE'}}}}), flush=True)
    print(json.dumps({{'type':'turn.completed'}}), flush=True)
time.sleep(1.2)
"""
    try:
        record = supervisor().execute(fixture_plan(tmp_path, code), tmp_path / "result.md")
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
    signals = []
    process = type("Process", (), {"pid": root_pid, "poll": lambda self: None,
                                   "wait": lambda self, timeout: None})()

    def environment(pid):
        if pid != owner_pid:
            return ()
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
    assert not module._is_nested_fabric_owner(row)


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


def test_reparented_marker_owner_with_valid_record_is_spared(tmp_path, monkeypatch):
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
        assert owner.identity in tracker.spared
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
    assert not module._is_nested_fabric_owner(row)


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

    monkeypatch.setattr(module.subprocess, "run", locale_ps)
    assert module._is_nested_fabric_owner(row)
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

    monkeypatch.setattr(module.subprocess, "run", canonical_ps)
    assert module._is_nested_fabric_owner(row)
    assert calls == ["C"]


@pytest.mark.parametrize("stop", ["normal", "cancelled"])
def test_nested_fabric_owner_and_its_child_are_spared(tmp_path, stop):
    inner = """import pathlib,subprocess,time
pathlib.Path('inner-ready').touch()
child = subprocess.Popen(['sleep', '60'], start_new_session=True,
    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path('inner-child.pid').write_text(str(child.pid))
time.sleep(2.5)
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
    plan = fixture_plan(tmp_path, outer, timeout_seconds=8, idle_seconds=8)
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
def test_all_adapters_stall_with_durable_diagnostics(tmp_path, adapter):
    plan = fixture_plan(
        tmp_path,
        "import time; time.sleep(30)",
        adapter,
        idle_seconds=0.2,
        timeout_seconds=3,
    )
    record = supervisor().execute(
        plan,
        tmp_path / "result.md",
        env={**os.environ, "CF_DISPATCH_ENABLE_COPILOT": "1"},
    )
    assert record["status"] == "stalled"
    assert record["exit"] is not None
    assert "idle_warning" in (tmp_path / "events.jsonl").read_text()
    assert record["evidence"]["signature"] == "idle_watchdog"


def test_silent_provider_fails_as_startup_timeout_and_is_reaped(tmp_path):
    child_pid = tmp_path / "child.pid"
    code = f"""import pathlib,subprocess,sys,time
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])
pathlib.Path({str(child_pid)!r}).write_text(str(child.pid))
print('opencode banner', file=sys.stderr, flush=True)
time.sleep(60)
"""
    plan = fixture_plan(tmp_path, code, "opencode", startup_seconds=0.6, idle_seconds=30, timeout_seconds=30)
    started = time.monotonic()
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert time.monotonic() - started < 10
    assert record["status"] == "startup_timeout"
    assert record["evidence"]["signature"] == "startup_watchdog"
    assert record["retryable"] is True
    assert record["fix"] == "re-route to another model or adapter"
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


def test_provider_output_before_startup_window_keeps_running(tmp_path):
    code = """import json,time
print(json.dumps({'type':'thread.started','thread_id':'t1'}), flush=True)
time.sleep(1.5)
print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':'DONE'}}), flush=True)
print(json.dumps({'type':'turn.completed'}), flush=True)
"""
    plan = fixture_plan(tmp_path, code, startup_seconds=0.6, idle_seconds=30, timeout_seconds=30)
    record = supervisor().execute(plan, tmp_path / "result.md")
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
    plan = fixture_plan(tmp_path, code, "cursor", timeout_seconds=2, idle_seconds=1)
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
time.sleep(0.6)
emit({'type':'assistant','message':{'content':[{'type':'text','text':'working'}]}})
time.sleep(0.6)
emit({'type':'result','subtype':'success','result':'DONE','is_error':False,'num_turns':3})
"""
    plan = fixture_plan(tmp_path, code, "claude", timeout_seconds=10, idle_seconds=5)
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert (tmp_path / "result.md").read_text() == "DONE"


def test_claude_resume_preamble_does_not_start_terminal_grace(tmp_path):
    # The real turn can start later than the grace period after the empty
    # preamble result, for example while a large session reloads.
    code = """import json,time
emit=lambda e: print(json.dumps(e), flush=True)
emit({'type':'system','subtype':'init','model':'claude-opus-5-5','session_id':'s-1'})
emit({'type':'result','subtype':'success','result':'','is_error':False,'num_turns':0})
time.sleep(0.8)
emit({'type':'system','subtype':'init','model':'claude-opus-5-5','session_id':'s-1'})
emit({'type':'assistant','message':{'content':[{'type':'text','text':'working'}]}})
emit({'type':'result','subtype':'success','result':'DONE','is_error':False,'num_turns':3})
"""
    plan = fixture_plan(tmp_path, code, "claude", timeout_seconds=10, idle_seconds=5)
    plan["grace_seconds"] = 0.3
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "ok"
    assert (tmp_path / "result.md").read_text() == "DONE"


def test_wall_timeout_is_distinct_from_idle(tmp_path):
    plan = fixture_plan(
        tmp_path, "import time; time.sleep(30)", idle_seconds=3, timeout_seconds=0.2
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


def test_catalogued_observed_substitution_records_answering_family_without_certifying(tmp_path):
    code = """import json
events = [
    {"type":"system","subtype":"init","model":"claude-haiku-4-5-20251001","session_id":"s-1"},
    {"type":"assistant","message":{"model":"claude-sonnet-5-5","content":[{"type":"text","text":"DONE"}]}},
    {"type":"result","result":"DONE","is_error":False,"session_id":"s-1"},
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
    assert record["provenance"]["observed_model"] == "claude-sonnet-5-5"
    assert record["provenance"]["family"] == "anthropic"
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


def test_ambiguous_catalogued_observed_substitution_keeps_family_unknown(tmp_path):
    code = """import json
events = [
    {"type":"system","subtype":"init","model":"claude-haiku-4-5-20251001","session_id":"s-1"},
    {"type":"assistant","message":{"model":"gpt-claude-model","content":[{"type":"text","text":"DONE"}]}},
    {"type":"result","result":"DONE","is_error":False,"session_id":"s-1"},
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
    assert record["provenance"]["observed_model"] == "gpt-claude-model"
    assert record["provenance"]["family"] == "unknown"
    assert not record["cross_family"]
    assert not record["certification_eligible"]


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
            "workhorse",
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
        alias="workhorse",
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
    bindir = tmp_path / "bin"
    bindir.mkdir()
    cli = bindir / "claude"
    initial_model = "opus" if explicit else "sonnet"
    cli.write_text("""#!/usr/bin/env python3
import json,sys
sys.stdin.read()
model=sys.argv[sys.argv.index('--model')+1]
print(json.dumps({'type':'result','is_error':model=='TARGET_MODEL','result':"You've hit your usage limit" if model=='TARGET_MODEL' else 'DONE'}))
sys.exit(1 if model=='TARGET_MODEL' else 0)
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
            "opus" if explicit else "workhorse",
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
    assert json.loads((run / "RUN_RECEIPT.json").read_text())["status"] == "ok"
    cooldowns = json.loads((tmp_path / "cooldowns.json").read_text())["cooldowns"]
    assert cooldowns["claude/*"]["source_run"] == rows[0]["run_id"]


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
        "if sys.argv[1] == 'export':\n"
        f"    pathlib.Path({str(pwd_file)!r}).write_text(os.environ['PWD'])\n"
        "    print(json.dumps({'messages':[{'info':{'providerID':'opencode-go','modelID':'deepseek-v4.1-flash'}}]}))\n"
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
    assert record["provenance"]["observed_source"] == "opencode:export.modelID"
    assert pwd_file.read_text() == str(child)


def test_capture_is_bounded_but_result_is_complete(tmp_path, monkeypatch):
    module = supervisor()
    path = tmp_path / "events.jsonl"
    capture = module.BoundedCapture(path, limit=100)
    capture.write(b"A" * 80)
    capture.write(b"B" * 80)
    capture.close()
    assert path.read_bytes() == b"A" * 50 + b"B" * 50


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
    git(tmp_path, "init", "-q", "-b", "main", str(repo))
    git(repo, "commit", "-q", "--allow-empty", "-m", "initial")
    lane, other = repo / ".worktrees/lane", repo / ".worktrees/other"
    git(repo, "worktree", "add", "-q", "-b", "lane", str(lane))
    git(repo, "worktree", "add", "-q", "-b", "other", str(other))
    return repo, lane, other


def codex_writer_plan(lane, workspace, resume=None, **controls):
    return supervisor().build_plan(
        "codex", {"resolved_model": "fixture"}, "hello", mode="worktree_write",
        worktree=lane, workspace_root=workspace, resume_session=resume, **controls,
    )


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
    assert plan["applied"]["write_boundary"]["filesystem"] == {str(path): access for path, access in table.items()}
    for granted in (private, private / "index", private / "index.lock", private / "HEAD",
                    private / "ORIG_HEAD", private / "logs/HEAD",
                    common / "objects", common / "refs/heads/lane", common / "logs/refs/heads/lane",
                    common / "packed-refs", common / "packed-refs.lock", common / "packed-refs.new"):
        assert codex_access(table, granted) == "write", granted
    for denied in (common, common / "hooks", common / "hooks/pre-commit", common / "config",
                   common / "info/exclude", common / "worktrees", sibling, sibling / "HEAD",
                   private / "config.worktree", private / "commondir", private / "gitdir", lane / ".git"):
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
    git(tmp_path, "init", "-q", "-b", "main", str(repo))
    (repo / SKILL).parent.mkdir(parents=True)
    (repo / SKILL).write_text("v1\n")
    (repo / "app.txt").write_text("app\n")
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "initial")
    lane = tmp_path / "lane"
    git(repo, "worktree", "add", "-q", "-b", "lane", str(lane))
    (lane / "app.txt").write_text("lane\n")
    git(lane, "commit", "-q", "-am", "lane work")
    (repo / SKILL).write_text("v2\n")
    git(repo, "commit", "-q", "-am", "main updates the skill")
    return repo, lane


def lane_attempt(tmp_path, lane, script, policy=None, adapter="codex"):
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
    return supervisor().execute(plan, run_dir / "result.md")


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
    assert record["error"] == "protected_instructions_changed"
    assert record["evidence"]["signature"] == "protected_instructions_changed"
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
    assert record["status"] == "ok", record
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
    assert record["status"] == "ok", record
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
    assert record["status"] == "ok", record
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
    assert record["status"] == "ok", record
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
    assert record["status"] == "ok", record
    assert not (lane / ".agents/local").is_symlink()
    assert (lane / notes).read_text() == "start notes\n"
    assert any("quarantined" in warning and notes in warning for warning in record["warnings"]), record["warnings"]


def test_quarantine_archives_a_staged_change_the_disk_no_longer_shows(tmp_path):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, f"open({SKILL!r}, 'w').write('staged lane\\n'); git('add', {SKILL!r})\n"
                          f"open({SKILL!r}, 'w').write('v1\\n')\n")
    assert record["status"] == "ok", record
    assert git(lane, "show", ":" + SKILL) == "v1\n"
    archived = [warning for warning in record["warnings"] if "quarantined" in warning]
    assert archived and "protected.index.patch" in archived[0], record["warnings"]
    assert "staged lane" in (tmp_path / "attempt" / "protected.index.patch").read_text()


@pytest.mark.parametrize("policy, status", [(None, "ok"), ({"instruction_changes": "deny"}, "failed")])
def test_a_merge_that_discards_integration_instruction_changes_is_caught(tmp_path, policy, status):
    _, lane = instruction_lane(tmp_path)
    git(lane, "reset", "-q", "--hard", "HEAD~1")  # the lane starts from main's old commit
    record = lane_attempt(tmp_path, lane, "git('merge', '-q', '-s', 'ours', '--no-edit', 'main')\n", policy=policy)
    assert record["status"] == status, record
    if status == "ok":
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
    assert record["status"] == "ok", record
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
    assert record["status"] == "ok", record
    assert (lane / SKILL).read_text() == "v1\n"
    assert any("instruction_changes" in warning for warning in record["warnings"])


@pytest.mark.parametrize("policy", [None, {"instruction_changes": "allow"}], ids=["default", "allow"])
def test_case_variant_and_special_instruction_changes_still_fail(tmp_path, policy):
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, "os.mkfifo('.agents/skills/fixture/pipe')\n", policy=policy)
    assert record["status"] == "failed"
    assert record["error"] == "protected_instructions_changed"


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


def test_codex_writer_unresolved_instruction_conflict_fails(tmp_path):
    _, lane = instruction_lane(tmp_path)
    (lane / SKILL).write_text("branch edit\n")
    git(lane, "commit", "-q", "-am", "reviewed skill edit")
    record = lane_attempt(tmp_path, lane, f"subprocess.run(['git', *{GIT_FIXTURE!r}, 'merge', '-q', 'main'])\n")
    assert record["error"] == "protected_instructions_changed"
    assert any(SKILL + " (unresolved conflict)" in warning for warning in record["warnings"])


@pytest.mark.parametrize("script", [
    "os.makedirs('.agents/hidden'); open('.agents/hidden/x', 'w').write('x'); os.chmod('.agents/hidden', 0)\n",
    "import shutil; shutil.copytree('.agents', '../elsewhere'); shutil.rmtree('.agents'); "
    "os.symlink('../elsewhere', '.agents')\n",
    "open('.agents/big', 'wb').truncate(1 << 40)\n",
], ids=["unreadable", "root-symlink", "oversized"])
def test_codex_writer_hiding_instructions_fails(tmp_path, script):
    _, lane = instruction_lane(tmp_path)
    hidden = lane / ".agents" / "hidden"
    try:
        record = lane_attempt(tmp_path, lane, script)
    finally:
        if hidden.exists():
            hidden.chmod(0o755)
    assert record["error"] == "protected_instructions_changed"
    assert any("unverifiable" in warning for warning in record["warnings"])


def test_instruction_check_refuses_while_lane_processes_run(tmp_path, monkeypatch):
    module = supervisor()
    original = module._Descendants.stop

    def stop_sparing_a_process(self, *args, **kwargs):
        reaped = original(self, *args, **kwargs)
        self.spared_at_stop.add((999999, "fixture"))
        return reaped

    monkeypatch.setattr(module._Descendants, "stop", stop_sparing_a_process)
    _, lane = instruction_lane(tmp_path)
    record = lane_attempt(tmp_path, lane, "", policy={"instruction_changes": "allow"})
    assert record["error"] == "protected_instructions_changed"
    assert any("left running" in warning for warning in record["warnings"])


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
            str(SCRIPTS / "cf_dispatch.sh"),
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


def test_opencode_argv_always_pins_plan_cwd(tmp_path):
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
    assert argv[argv.index("--dir") + 1] == str(tmp_path)


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
    parsed = supervisor().parse_output("kiro", "\n".join(map(json.dumps, events)))
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


def test_writer_watchdog_does_not_count_its_own_warning_as_progress(tmp_path):
    plan = fixture_plan(
        tmp_path,
        "import time; time.sleep(30)",
        mode="worktree_write",
        idle_seconds=1.2,
        timeout_seconds=3.5,
    )
    record = supervisor().execute(plan, tmp_path / "result.md")
    assert record["status"] == "stalled"

@pytest.mark.parametrize('api_key', [False, True])
def test_claude_plan_isolates_settings_and_mcp(tmp_path, monkeypatch, api_key):
    monkeypatch.delenv('ANTHROPIC_BASE_URL', raising=False)
    monkeypatch.delenv('ANTHROPIC_AUTH_TOKEN', raising=False)
    monkeypatch.delenv('ANTHROPIC_API_KEY', raising=False)
    if api_key:
        monkeypatch.setenv('ANTHROPIC_API_KEY', 'fixture-key')
    plan = supervisor().build_plan('claude', {'resolved_model': 'opus'}, 'hello', cwd=tmp_path, workspace_root=tmp_path)
    assert ('--bare' if api_key else '--safe-mode') in plan['argv']
    assert '--strict-mcp-config' in plan['argv']
    assert plan['argv'][plan['argv'].index('--permission-mode') + 1] == 'default'
    assert plan['argv'][plan['argv'].index('--tools') + 1] == 'Read,Grep,Glob'
    assert 'fixture-key' not in ' '.join(plan['argv'])

@pytest.mark.parametrize('adapter', ['claude', 'codex', 'opencode'])
def test_successful_answer_survives_tool_permission_diagnostic(adapter):
    parsed = supervisor().parse_output(adapter, '{"type":"result","result":"DONE"}', 'ls: /root: Permission denied', 0)
    assert parsed['status'] == 'ok'


def test_question_example_in_middle_of_answer_does_not_suspend():
    parsed = supervisor().parse_output('claude', json.dumps({'type':'result','result':'Example:\n```\nQUESTION: Which?\n```\nEnd of explanation.'}))
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
    monkeypatch.setattr(mod, '_sandbox_exec_path', lambda: '/usr/bin/sandbox-exec')
    plan = mod.build_plan(adapter, {}, 'hello', cwd=tmp_path, workspace_root=tmp_path,
                          mode='worktree_write', add_dirs=[add_dir])
    assert str(add_dir) in plan['applied']['add_dirs']
    assert str(add_dir) in plan['applied']['write_boundary']['writable_paths']


def test_opencode_does_not_claim_unsupported_add_dirs_without_wrapper(monkeypatch, tmp_path):
    mod = supervisor()
    monkeypatch.setattr(mod, '_sandbox_exec_path', lambda: None)
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


def test_failure_classification_uses_diagnostics_not_answer_prose():
    parsed = supervisor().parse_output('opencode', 'Explain the rate limit algorithm', '', 1)
    assert parsed['status'] == 'failed'
    assert supervisor().parse_output('claude', '', '529 overloaded', 1)['status'] == 'rate_limited'


def test_kiro_tool_echo_does_not_override_observed_model():
    output = '\n'.join(map(json.dumps, [
        {'type':'system', 'model':'actual'},
        {'type':'tool_result','data':{'model':'echoed'}},
        {'type':'result','result':'DONE'},
    ]))
    assert supervisor().parse_output('kiro', output)['observed_model'] == 'actual'


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
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048)
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
    assert plan['argv'][plan['argv'].index('--variant') + 1] == 'medium'


def test_fallback_uses_router_candidates_and_parses_explicit_routes():
    mod = importlib.import_module('skills.orchestrate.scripts.exec_routing')
    plan = {'adapter':'claude','model':'opus','effort':'high','route':{'fallback_candidates':[{'adapter':'codex','model':'gpt-6.1-sol','effort_applied':'medium'}]}}
    assert mod.candidates(plan, True, {}) == [{'adapter':'codex','model':'gpt-6.1-sol','effort':'medium'}]
    assert mod.candidates(plan, ['codex/gpt-6.1-sol@low'], {}) == [{'adapter':'codex','model':'gpt-6.1-sol','effort':'low'}]


@pytest.mark.parametrize('policy', ['yes', 'codex/gpt-6.1-sol', 3, {}, [''], [3], [{'adapter': [], 'model': 'sol'}]])
def test_invalid_fallback_rejected_during_preflight(tmp_path, monkeypatch, policy):
    mod = importlib.import_module('skills.orchestrate.scripts.dispatch_run')
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
    first_pass = scanner.completed_pass_started_at
    last = list(os.walk(directory))[0][2][-1]
    (directory / last).write_text('changed')
    # A slow host may need many probes; one full later pass is the real bound.
    for _ in range(60001):
        changed = scanner.probe()
        assert scanner.last_visited <= 2000
        if changed or scanner.completed_pass_started_at != first_pass:
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


def test_writer_stall_waits_for_full_idle_interval_scan(tmp_path):
    plan = fixture_plan(tmp_path, 'import time; time.sleep(30)',
                        mode='worktree_write', idle_seconds=1.2, timeout_seconds=1.6)
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
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048)
    code = '''import json
print(json.dumps({'type':'result','is_error':True,'result':'backend exploded'}))
for i in range(1000): print(json.dumps({'type':'telemetry','data':'x'*100}))
'''
    record = mod.execute(fixture_plan(tmp_path, code, 'claude'), tmp_path / 'result.md')
    assert record['status'] == 'failed'


def test_explicit_route_fallback_policy_reaches_router(tmp_path, monkeypatch):
    mod = importlib.import_module('skills.orchestrate.scripts.dispatch_run')
    monkeypatch.chdir(tmp_path)
    monkeypatch.setenv('AGENT_FABRIC_PRODUCT_ROOT', str(ROOT))
    monkeypatch.setenv('AGENT_FABRIC_INSTANCE_ROOT', str(ROOT))
    result = mod.preflight_tasks([{'id':'one','adapter':'claude','model':'opus','prompt':'hello','fallback':True}])
    assert result['status'] == 'validated'
    assert result['routes'][0]['fallback_candidates']


def test_oversized_plain_result_is_partial_and_not_certifiable(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048)
    plan = fixture_plan(tmp_path, "print('a' * 10000)", intent='assurance', orchestrator_family='anthropic')
    record = mod.execute(plan, tmp_path / 'result.md')
    assert record['status'] == 'partial'
    assert not record['certification_eligible']
    assert any('truncated' in warning for warning in record['warnings'])
    assert (tmp_path / 'result.md').stat().st_size <= 2048


def test_oversized_split_result_cannot_certify_preliminary_text(tmp_path, monkeypatch):
    mod = supervisor()
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048)
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
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048 if rollover else 20*1024*1024)
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
    monkeypatch.setattr(mod, 'MAX_EVENTS_BYTES', 2048 if rollover else 20*1024*1024)
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
    parsed = supervisor().parse_output("agy", "\n".join(map(json.dumps, events)))
    assert parsed["status"] == "ok"
    assert parsed["text"] == "PONG\n"
    assert parsed["observed_model"] == "gemini-3.8-flash-high"


def test_agy_nested_result_failure_keeps_provider_error():
    events = [
        {"event": "result", "result": {"status": "FAILED", "response": "",
         "error": "RESOURCE_EXHAUSTED (code 429): Individual quota reached. Resets in 29m44s."}},
    ]
    parsed = supervisor().parse_output("agy", "\n".join(map(json.dumps, events)), exit_code=3)
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
    assert supervisor()._same_model(adapter, resolved, observed) is same


def test_kiro_v2_engine_stream_is_parsed_and_auto_is_not_passed():
    # Shape captured from a live kiro-cli --agent-engine v2 run (2026-09-23).
    events = [
        {"type": "runStarted", "data": {"payloadSchema": "acp", "engine": "v2"}},
        {"type": "sessionUpdate", "data": {"sessionId": "k2", "update": {
            "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "PONG"}}}},
        {"type": "runFinished", "data": {"sessionId": "k2", "status": "success",
         "stopReason": "end_turn", "finalText": "PONG"}},
    ]
    parsed = supervisor().parse_output("kiro", "\n".join(map(json.dumps, events)))
    assert (parsed["status"], parsed["text"], parsed["session_id"]) == ("ok", "PONG", "k2")
    from adapters import kiro
    command = kiro.argv({"mode": "read_only", "resume_session": None, "model": "auto",
                         "effort": None, "boundary_prompt": "B", "prompt": "P"})
    assert "--model" not in command


def test_claude_reported_ids_match_their_aliases():
    module = supervisor()
    assert module._same_model("claude", "haiku", "claude-haiku-4-5-20251001")
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
    sandbox_exec = mod._sandbox_exec_path()
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
    result = subprocess.run([sandbox_exec, "-p", mod.os_confinement_profile(plan), "/bin/sh", "-c", script],
                            cwd=lane, capture_output=True, text=True)
    if result.returncode and "sandbox_apply" in result.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert result.returncode == 0, result.stderr
    assert (lane / SKILL).read_text() == "v2\n"
    assert (cache / "node/corepack").is_dir()


def first_instruction_lane(tmp_path):
    """A lane cut before main adds the repository's first skill."""
    repo = tmp_path / "repo"
    git(tmp_path, "init", "-q", "-b", "main", str(repo))
    (repo / "app.txt").write_text("app\n")
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "initial")
    lane = tmp_path / "lane"
    git(repo, "worktree", "add", "-q", "-b", "lane", str(lane))
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
    assert record["status"] == "ok", record
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
        'permissions.NAME.filesystem={":tmpdir" = "write", ' + json.dumps(str(locks)) + ' = "write"}',
    ]
    assert "-s" not in plan["argv"]
    assert f"codex read_only add-dir stays read-only (it holds cwd or a pattern character): {repo}" in plan["warnings"]
    networked = supervisor().build_plan("codex", {"resolved_model": "fixture"}, "hello", cwd=repo,
                                        workspace_root=repo, network=True, resume_session="saved")
    assert "permissions.NAME.network.enabled=true" in codex_read_only_permissions(networked["argv"])


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
    sandbox_exec = mod._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    repo, _, plan = opencode_config_repo(tmp_path)
    profile = mod.os_confinement_profile(plan)

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


@pytest.mark.skipif(sys.platform != "darwin" or not shutil.which("opencode"), reason="needs OpenCode")
def test_opencode_starts_below_a_repository_config(tmp_path):
    mod = supervisor()
    sandbox_exec = mod._sandbox_exec_path()
    if not sandbox_exec:
        pytest.skip("sandbox-exec is unavailable or disabled")
    _, cwd, plan = opencode_config_repo(tmp_path)
    (cwd.parents[1] / "opencode.jsonc").unlink()
    attempt = tmp_path / "attempt"
    (attempt / "tmp/cache").mkdir(parents=True)
    plan["run_dir"] = str(attempt)
    result = subprocess.run(
        [sandbox_exec, "-p", mod.os_confinement_profile(plan), "opencode", "debug", "config"],
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
    if sys.platform != "darwin" or not mod._sandbox_exec_path():
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
    probe = subprocess.run([mod._sandbox_exec_path(), "-p", mod.os_confinement_profile(plan), "/usr/bin/true"],
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
    assert codex_read_only_permissions(plan["argv"])[-1] == 'permissions.NAME.filesystem={":tmpdir" = "write"}'
    assert any(warning.endswith(str(literal)) and "pattern character" in warning for warning in plan["warnings"])


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
    files, _, directories = supervisor()._toolchain_reads(plan, repo, search_path)
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
        assert granted != home and not supervisor().credential_path(granted)
        assert not (home / ".local").is_relative_to(granted)


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
    files, project_files, directories = supervisor()._toolchain_reads(plan, repo, "/nonexistent")
    assert repo.resolve() / ".venv" in directories
    assert home / ".pyenv/versions/3.13.0/lib" in directories
    assert tools["python3"] in files
    assert not project_files  # no uv on PATH


@pytest.mark.skipif(sys.platform != "darwin", reason="sandbox-exec is macOS-only")
def test_confined_read_only_lane_runs_home_toolchains_but_not_credentials(tmp_path, monkeypatch):
    mod = supervisor()
    if not mod._sandbox_exec_path():
        pytest.skip("sandbox-exec is unavailable or disabled")
    home, tools, toolchain_path = fake_toolchain_home(tmp_path, monkeypatch)
    search_path = os.pathsep.join([toolchain_path, "/usr/bin", "/bin"])
    repo = tmp_path / "repo"
    repo.mkdir()
    attempt = tmp_path / "attempt"
    attempt.mkdir()
    plan = {"adapter": "claude", "mode": "read_only", "cwd": str(repo), "workspace_root": str(repo),
            "run_dir": str(attempt), "applied": {"confinement": "sandbox-exec", "add_dirs": []}}
    profile = mod.os_confinement_profile(plan, search_path)

    def run(command):
        return subprocess.run([mod._sandbox_exec_path(), "-p", profile, "/bin/sh", "-c", command], cwd=repo,
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
    planted = mod.os_confinement_profile(plan, str(home / ".ssh/bin"))
    blocked = subprocess.run([mod._sandbox_exec_path(), "-p", planted, str(home / ".ssh/bin/node")],
                             capture_output=True, text=True)
    assert blocked.returncode != 0 and "planted" not in blocked.stdout


@pytest.mark.skipif(sys.platform != "darwin" or not shutil.which("uv"), reason="needs sandbox-exec and uv")
def test_confined_read_only_lane_runs_uv_with_a_home_interpreter(tmp_path):
    mod = supervisor()
    if not mod._sandbox_exec_path():
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
    profile = mod.os_confinement_profile(plan, lane_path)
    result = subprocess.run(
        [mod._sandbox_exec_path(), "-p", profile, "uv", "run", "--frozen", "--offline", "python", "-c", "print(1)"],
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
        listing = subprocess.run([mod._sandbox_exec_path(), "-p", profile, "/bin/ls", str(ssh)],
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
    assert granted_outside(repo, supervisor()._toolchain_reads(plan, repo, "/nonexistent")) == []


def test_venv_link_to_a_credential_file_grants_nothing(tmp_path, monkeypatch):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    secret = fake_executable(home / ".git-credentials", "https://user:token@example.invalid\n")
    repo, plan = venv_repo(tmp_path, python_target=secret)
    assert supervisor().credential_path(secret)
    assert granted_outside(repo, supervisor()._toolchain_reads(plan, repo, "/nonexistent")) == []


def test_venv_link_to_a_non_toolchain_folder_grants_nothing(tmp_path, monkeypatch):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    target = fake_executable(home / "Documents/project/bin/python3")  # no lib/python3.* beside it
    repo, plan = venv_repo(tmp_path, python_target=target)
    assert granted_outside(repo, supervisor()._toolchain_reads(plan, repo, "/nonexistent")) == []


def test_interpreter_installed_directly_in_home_grants_nothing(tmp_path, monkeypatch):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    target = fake_executable(home / "bin/python3")
    (home / "lib/python3.13").mkdir(parents=True)
    repo, plan = venv_repo(tmp_path, python_target=target, home_line=str(home / "bin"))
    assert granted_outside(repo, supervisor()._toolchain_reads(plan, repo, str(home / "bin"))) == []


def test_venv_link_to_a_real_install_grants_its_runtime_components(tmp_path, monkeypatch):
    home, tools, _ = fake_toolchain_home(tmp_path, monkeypatch)
    repo, plan = venv_repo(tmp_path, python_target=tools["python3"], home_line=str(tools["python3"].parent))
    prefix = home / ".pyenv/versions/3.13.0"
    assert granted_outside(repo, supervisor()._toolchain_reads(plan, repo, "/nonexistent")) == [
        tools["python3"], prefix / "lib", prefix / "include"
    ]


@pytest.mark.parametrize("name", ["python3.14t", "python3t", "python3.13"])
def test_free_threaded_interpreter_is_a_toolchain(tmp_path, monkeypatch, name):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    prefix = home / ".local/share/uv/python/cpython-3.14.0t-macos-aarch64-none"
    interpreter = fake_executable(prefix / "bin" / name)
    (prefix / "lib/python3.14t").mkdir(parents=True)
    assert supervisor()._toolchain_grant(interpreter) == ([interpreter], [prefix / "lib"])


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
    files, directories = supervisor()._toolchain_grant(interpreter)
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
    assert supervisor()._toolchain_grant(interpreter) == (
        [interpreter], [prefix / "lib", prefix / "include"])
    # Even a link that stays inside the prefix is left out: only a real entry is canonical.
    (prefix / "Python").unlink()
    (prefix / "lib/Python").write_bytes(b"dylib")
    (prefix / "Python").symlink_to(prefix / "lib/Python")
    assert supervisor()._toolchain_grant(interpreter)[0] == [interpreter]


@pytest.mark.parametrize("component", ["Resources", "include", "libexec"])
def test_runtime_directory_linked_to_the_prefix_itself_is_not_granted(tmp_path, monkeypatch, component):
    home, _, _ = fake_toolchain_home(tmp_path, monkeypatch)
    prefix, interpreter = fake_framework_python(home)
    if (prefix / component).exists():
        shutil.rmtree(prefix / component)
    (prefix / component).symlink_to(".")
    assert (prefix / component).resolve() == prefix
    files, directories = supervisor()._toolchain_grant(interpreter)
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
    assert supervisor()._toolchain_grant(interpreter) == ([], [])


def test_runtime_component_must_lie_strictly_inside_the_prefix(tmp_path):
    mod = supervisor()
    prefix = tmp_path / "prefix"
    (prefix / "lib").mkdir(parents=True)
    (prefix / "Python").write_bytes(b"dylib")
    (prefix / "Resources").symlink_to(".")
    (prefix / "include").symlink_to(tmp_path)
    assert mod._runtime_component(prefix, "lib", directory=True) == prefix / "lib"
    assert mod._runtime_component(prefix, "Python", directory=False) == prefix / "Python"
    for name in ("Resources", "include", ".", ""):
        assert mod._runtime_component(prefix, name, directory=True) is None, name
    assert mod._runtime_component(prefix, "lib", directory=False) is None


@pytest.mark.skipif(sys.platform != "darwin" or not all(map(shutil.which, ("install_name_tool", "codesign", "otool"))),
                    reason="needs sandbox-exec and the macOS binary tools")
def test_confined_read_only_lane_runs_a_framework_python_in_home(tmp_path, monkeypatch):
    mod = supervisor()
    if not mod._sandbox_exec_path():
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
    profile = mod.os_confinement_profile(plan, search_path)

    def run(*command):
        return subprocess.run([mod._sandbox_exec_path(), "-p", profile, *command], cwd=repo, capture_output=True,
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
    assert supervisor()._toolchain_grant(tools["python3"]) == ([tools["python3"]], [prefix / "lib"])


def test_toolchain_rules_emit_the_validated_path_without_resolving_again(tmp_path):
    mod = supervisor()
    target = tmp_path / "elsewhere"
    target.mkdir()
    link = tmp_path / "lib"
    link.symlink_to(target)
    assert mod._sbpl_rule("allow", "file-read-data", [link], canonical=True) \
        == f'(allow file-read-data (subpath "{link}"))\n'
    assert mod._sbpl_rule("allow", "file-read-data", [link], literal=True, canonical=True) \
        == f'(allow file-read-data (literal "{link}"))\n'
    assert str(target.resolve()) in mod._sbpl_rule("allow", "file-read-data", [link])


@pytest.mark.parametrize("secret", [
    ".git-credentials", ".netrc", ".npmrc", ".pypirc", ".pgpass", ".vault-token", ".docker/config.json",
    ".kube/config", ".aws/credentials", ".config/gh/hosts.yml", ".config/gcloud/credentials.db",
    ".azure/accessTokens.json", ".gnupg/private-keys-v1.d", ".ssh/id_ed25519", ".password-store/a.gpg",
    ".cargo/credentials.toml", ".gem/credentials", ".config/git/credentials", ".terraform.d/credentials.tfrc.json",
    ".boto", ".s3cfg", ".config/hub", ".config/op/config",
])
def test_credential_path_covers_common_host_secrets(tmp_path, monkeypatch, secret):
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    assert supervisor().credential_path(tmp_path / "home" / secret)
