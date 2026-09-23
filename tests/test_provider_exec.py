"""Provider supervisor gates use fixture programs only, never model calls."""

import importlib
import json
import os
from pathlib import Path
import signal
import subprocess
import ctypes
import sys
import time
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
    plan = {
        "adapter": "agy", "mode": "read_only", "workspace_root": str(root),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]},
    }
    monkeypatch.setattr(supervisor, "_sandbox_exec_path", lambda: "/usr/bin/sandbox-exec")
    profile = supervisor.os_confinement_profile(plan)
    assert '(deny file-read-data file-write* (subpath "' + str(root) + '")' in profile
    assert '(subpath "' + str(Path.home() / ".claude/projects") + '")' in profile
    assert '(subpath "' + str(Path.home() / ".codex/sessions") + '")' in profile
    assert '(allow file-read-data (subpath "' + str(cwd) + '") (subpath "' + str(add_dir) + '"))' in profile
    monkeypatch.setattr(supervisor, "os_confinement_profile", lambda _plan: "(version 1)")
    assert supervisor.confinement_command(plan, ["/bin/cat", "file"]) == [
        "/usr/bin/sandbox-exec", "-p", "(version 1)", "/bin/cat", "file",
    ]


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


def test_os_confinement_profile_places_workspace_denials_before_allows(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    home = tmp_path / "home"
    workspace = home
    add_dir = tmp_path / "T" / "extra"
    cwd = workspace / "repo"
    (home / ".gemini/config").mkdir(parents=True)
    cwd.mkdir(parents=True)
    add_dir.mkdir(parents=True)
    monkeypatch.setattr(supervisor.Path, "home", lambda: home)
    monkeypatch.setattr(supervisor, "_darwin_user_dirs", lambda: (tmp_path / "T",))
    monkeypatch.setattr(supervisor, "CONFINED_STATE", {
        "agy": {"read_write": (".gemini/config",), "read": ()},
    })
    monkeypatch.setattr(supervisor, "EXTRA_DENIED_READS", ("private/cache",))
    plan = {
        "adapter": "agy", "mode": "read_only", "workspace_root": str(workspace),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": [str(add_dir)]},
    }

    profile = supervisor.os_confinement_profile(plan).splitlines()

    assert profile[1] == "(allow default)"
    assert profile[2].startswith('(deny file-read-data ')
    assert profile[3].startswith('(deny file-write* ')
    assert profile[4] == f'(allow file-read-data file-write* (subpath "{tmp_path / "T"}"))'
    assert profile[5] == f'(deny file-read-data file-write* (subpath "{workspace}") (subpath "{add_dir}") (subpath "{home / "private/cache"}"))'
    assert profile[6].startswith(f'(allow file-read-data file-write* (subpath "{home / ".gemini/config"}")')
    assert profile[7] == f'(allow file-read-data (subpath "{cwd}") (subpath "{add_dir}"))'


def test_sbpl_filter_star_escapes_regex_without_resolving_symlink_target(tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    target = tmp_path / "target with [regex].json"
    target.write_text("secret", encoding="utf-8")
    link = tmp_path / "link with [regex].json"
    link.symlink_to(target)

    rule = supervisor._sbpl_filter(str(link) + "*")

    assert rule == f'(regex #"^{link.parent}/link with \\[regex\\]\\.json[^/]*$")'


def test_darwin_user_dirs_uses_absolute_getconf_and_retries_failures(monkeypatch):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    calls = []

    def run(argv, **kwargs):
        calls.append(argv)
        if len(calls) == 1:
            raise FileNotFoundError("temporary getconf failure")
        if len(calls) == 2:
            return SimpleNamespace(returncode=1, stdout="/private/var/folders/user/T/\n")
        return SimpleNamespace(returncode=0, stdout="/private/var/folders/user/T/\n")

    monkeypatch.setattr(supervisor.subprocess, "run", run)
    monkeypatch.setattr(supervisor, "_DARWIN_USER_DIRS_CACHE", None, raising=False)
    try:
        assert supervisor._darwin_user_dirs() == ()
        assert supervisor._darwin_user_dirs() == (
            Path("/private/var/folders/user/T"), Path("/private/var/folders/user/T"),
        )
        assert all(argv[0] == "/usr/bin/getconf" for argv in calls)
    finally:
        monkeypatch.setattr(supervisor, "_DARWIN_USER_DIRS_CACHE", None, raising=False)


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


def test_os_confinement_profile_denies_home_and_reallows_only_the_adapters_state(monkeypatch, tmp_path):
    supervisor = importlib.import_module("skills.orchestrate.scripts.provider_exec")
    home = tmp_path / "home"
    cwd = tmp_path / "workspace"
    cwd.mkdir()
    monkeypatch.setattr(supervisor.Path, "home", lambda: home)
    monkeypatch.setattr(supervisor, "_darwin_user_dirs", lambda: (tmp_path / "T",))
    monkeypatch.setattr(supervisor, "EXTRA_DENIED_READS", ("private/cache",))
    monkeypatch.setattr(supervisor, "CONFINED_STATE", {
        "agy": {"read_write": ("state", "creds.json*"), "read": ("keys/one",)},
    })
    plan = {
        "adapter": "agy", "mode": "read_only", "workspace_root": str(cwd),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }

    profile = supervisor.os_confinement_profile(plan).splitlines()

    assert profile[2] == f'(deny file-read-data (subpath "{home}") (subpath "/private/tmp"))'
    assert profile[3].startswith(f'(deny file-write* (subpath "{home}")')
    assert profile[4] == f'(allow file-read-data file-write* (subpath "{tmp_path / "T"}"))'
    assert profile[5] == f'(deny file-read-data file-write* (subpath "{cwd}") (subpath "{home / "private/cache"}"))'
    assert profile[6].startswith(f'(allow file-read-data file-write* (subpath "{home / "state"}") (regex #"^')
    assert profile[7] == f'(allow file-read-data (subpath "{home / "keys/one"}"))'
    assert profile[8] == f'(allow file-read-data (subpath "{cwd}"))'
    plan["adapter"] = "opencode"
    assert "keys/one" not in supervisor.os_confinement_profile(plan)


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
    monkeypatch.setattr(supervisor.Path, "home", lambda: home)
    monkeypatch.setattr(supervisor, "_darwin_user_dirs", lambda: ())
    monkeypatch.setattr(supervisor, "CONFINED_STATE", {"agy": {"read_write": ("state", "creds.json*"), "read": ()}})
    plan = {
        "adapter": "agy", "mode": "read_only", "workspace_root": str(home / "repo"),
        "cwd": str(cwd), "applied": {"confinement": "sandbox-exec", "add_dirs": []},
    }
    profile = supervisor.os_confinement_profile(plan)

    def run(script):
        return subprocess.run([sandbox_exec, "-p", profile, "/bin/sh", "-c", script], capture_output=True, text=True)

    probe = run(f"cat {cwd}/note.txt")
    if probe.returncode and "sandbox_apply" in probe.stderr:
        pytest.skip("sandbox_apply is refused in this test environment")
    assert probe.stdout == "note\n"
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
    monkeypatch.setattr(supervisor, "_darwin_user_dirs", lambda: (user_temp,))
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
    assert any("reads are unconfined" in warning for warning in unconfined["warnings"])


@pytest.mark.parametrize("adapter", ["codex", "claude", "cursor", "kiro", "agy", "opencode"])
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
        ('error: invalid model selection (--model "gemini-3.8-pro" --effort "high"): --effort is not supported for model "gemini-3.8-pro"', "model_unavailable"),
        ("invalid model selection; quota exceeded", "usage_limited"),
        ("permission denied", "permission_blocked"),
    ],
)
def test_failure_signatures(adapter, text, status):
    parsed = supervisor().parse_output(adapter, "", text, 1)
    assert parsed["status"] == status
    assert parsed["signature"]
    assert len(parsed["excerpt"]) <= 200


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
    assert exec_routing.model_families("claude-sonnet-5") == ("anthropic",)


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
    assert observed.read_text() == str(SCRIPTS / "bin/ps")


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


def test_attempt_marker_matches_inherited_environment_only():
    marker = "fixture-marker-123"
    environment = dict(os.environ)
    environment.pop("PROVENANT_ATTEMPT_MARKER", None)
    child = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(30)",
         "PROVENANT_ATTEMPT_MARKER=" + marker],
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        assert not supervisor()._has_attempt_marker(child.pid, marker)
    finally:
        child.terminate()
        child.wait(timeout=3)
    environment["PROVENANT_ATTEMPT_MARKER"] = marker
    child = subprocess.Popen(
        [sys.executable, "-c", "import time; time.sleep(30)"],
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
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
    {"type":"assistant","message":{"model":"claude-sonnet-5","content":[{"type":"text","text":"DONE"}]}},
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
    assert record["provenance"]["observed_model"] == "claude-sonnet-5"
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
    {"type":"assistant","message":{"model":"claude-sonnet-5","content":[{"type":"text","text":"DONE"}]}},
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
    cli.write_text("""#!/usr/bin/env python3
import json,sys
sys.stdin.read()
model=sys.argv[sys.argv.index('--model')+1]
print(json.dumps({'type':'result','is_error':model=='opus','result':"You've hit your usage limit" if model=='opus' else 'DONE'}))
sys.exit(1 if model=='opus' else 0)
""")
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
    assert (
        json.loads((tmp_path / "cooldowns.json").read_text())["cooldowns"][
            "claude/*"
        ]["source_run"]
        == rows[0]["run_id"]
    )


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


def test_linked_writer_automatically_grants_git_metadata(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    subprocess.run(["git", "init", str(repo)], check=True, capture_output=True)
    subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
        check=True,
        capture_output=True,
    )
    lane = tmp_path / "lane"
    subprocess.run(
        ["git", "-C", str(repo), "worktree", "add", "-b", "lane", str(lane)],
        check=True,
        capture_output=True,
    )
    plan = supervisor().build_plan(
        "codex",
        {"resolved_model": "fixture"},
        "hello",
        mode="worktree_write",
        worktree=lane,
        workspace_root=tmp_path,
        network=False,
    )
    assert str(repo / ".git") in plan["applied"]["add_dirs"]
    assert plan["applied"]["network"] is False
    assert "sandbox_workspace_write.network_access=false" in plan["argv"]
    assert "--ephemeral" not in plan["argv"]


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


def test_opencode_does_not_claim_unsupported_add_dirs(tmp_path):
    plan = supervisor().build_plan('opencode', {}, 'hello', cwd=tmp_path, workspace_root=tmp_path, add_dirs=[tmp_path])
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
    plan = {'adapter':'claude','model':'opus','effort':'high','route':{'fallback_candidates':[{'adapter':'codex','model':'gpt-6-sol','effort_applied':'medium'}]}}
    assert mod.candidates(plan, True, {}) == [{'adapter':'codex','model':'gpt-6-sol','effort':'medium'}]
    assert mod.candidates(plan, ['codex/gpt-6-sol@low'], {}) == [{'adapter':'codex','model':'gpt-6-sol','effort':'low'}]


@pytest.mark.parametrize('policy', ['yes', 'codex/gpt-6-sol', 3, {}, [''], [3], [{'adapter': [], 'model': 'sol'}]])
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


def test_kiro_stream_json_selects_the_v2_engine():
    from adapters import kiro
    command = kiro.argv({"mode": "read_only", "resume_session": None, "model": "auto",
                         "effort": None, "boundary_prompt": "B", "prompt": "P"})
    assert command[command.index("--agent-engine") + 1] == "v2"
    assert command.index("--agent-engine") < command.index("--output-format")


@pytest.mark.parametrize(
    "adapter,resolved,observed,same",
    [
        ("claude", "opus", "claude-opus-5-5", True),
        ("cursor", "grok-4.7", "Grok 4.7 256K High Fast", True),
        ("cursor", "auto", "Auto", True),
        ("codex", "gpt-6-luna", "gpt-6-sol", False),
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
    assert module._same_model("claude", "sonnet", "claude-sonnet-5")
    assert not module._same_model("claude", "haiku", "claude-sonnet-5")


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
