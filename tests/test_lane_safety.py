"""Lane-safety hooks: kill guard tokeniser, reap, pre-push guard and settings merge."""

from __future__ import annotations

import json
import os
import shlex
import subprocess
import sys
import time
from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
SCRIPTS = ROOT / "scripts"


def load(name: str, filename: str):
    return load_script(SCRIPTS / filename, name, register=True)


guard = load("lane_safety_guard_test", "lane_safety_guard.py")
reap = load("reap_test", "reap.py")
hooks = load("configure_claude_hooks_test", "configure-claude-hooks.py")

DENIED = [
    "pkill -f server",
    'echo "Stopped user\'s server: $(pkill -f server)"',
    "killall node",
    "/usr/bin/pkill node",
    "echo done; pkill node",
    "make && pkill node",
    "make || killall node",
    "ps aux | pkill -f x",
    "(pkill node)",
    "{ pkill node; }",
    "sudo pkill node",
    "sudo -u root killall node",
    "env FOO=1 pkill node",
    "nice -n 5 killall node",
    "timeout 5 pkill node",
    "timeout -s KILL 5 killall node",
    "nohup pkill node",
    "bash -c 'pkill node'",
    "sh -c \"killall node\"",
    "zsh -lc 'pkill node'",
    "sudo env X=1 bash -c 'pkill node'",
    "bash -c \"bash -c 'pkill node'\"",
    "'pk'ill node",
    "\\pkill node",
    "eval 'pkill node'",
    "echo $(pkill node)",
    "echo `killall node`",
    "kill $(pgrep -f node)",
    "kill `pgrep node`",
    "kill -9 $(pgrep node)",
    "pgrep node | xargs kill",
    "pgrep node | xargs -n1 kill -9",
    "kill -- -4242",
    "kill -9 -- -4242",
    "kill -TERM -4242 -4243",
    "kill 0",
    "kill -9 0",
    "kill -TERM -4242",
    "kill -s TERM -4242",
    "kill -9 -4242",
    "kill --signal=TERM -- -4242",
    "sudo --user root pkill node",
    "sudo --group wheel killall node",
    "env --unset=X pkill node",
    "env -u X pkill node",
    "nice --adjustment=5 pkill node",
    "timeout --signal=KILL 5 pkill node",
    "echo ${X:-$(pkill node)}",
    "echo ${X:-`killall node`}",
    "timeout --signal TERM 5 kill $(pgrep -f server)",
    "timeout -s TERM -k 3 5 pkill node",
    "timeout --kill-after=3 5 kill $PID",
    'echo "$(pkill node)"',
    "kill $PID",
    "kill ${PID}",
    "kill %1",
    "kill abc",
    "kill -s TERM abc",
    "kill 12 abc",
    "for p in 1 2; do kill $p; done",
    "find . -name x -exec kill {} \\;",
    "bash <<EOF\npkill node\nEOF",
    "bash <<< 'pkill node'",
    "cat <<EOF\n$(pkill node)\nEOF",
    "echo hi\npkill node",
]

ALLOWED = [
    "",
    'echo "$(rg \'sudo pkill\' docs)"',
    "ls -la",
    "kill 4242",
    "kill -9 4242 4243",
    "kill -TERM 4242",
    "kill -s KILL 4242",
    "kill -0 4242",
    "kill -l",
    "sudo kill 4242",
    "env X=1 kill 4242",
    "FOO=1 kill 4242",
    "scripts/reap.py 4242 --tree",
    "pgrep -f node",
    "echo pkill",
    "echo 'pkill node; kill $(pgrep x)'",
    "grep pkill notes.txt",
    "man pkill",
    "rg 'sudo pkill' docs",
    "git log --grep='command pkill'",
    "echo 'foo; pkill node'",
    'echo "foo; pkill node"',
    "timeout --signal TERM 5 kill 4242",
    "which killall",
    "git commit -m 'never pkill or kill -- -1'",
    "ls # pkill node",
    "cat <<'EOF'\npkill node\nEOF",
    "cat <<'EOF'\n$(pkill node)\nEOF",
    "cat <<EOF\nplain text\nEOF",
    "bash -c 'echo hi'",
    "bash script.sh",
    "sleep 5 & wait $!",
    "while kill -0 4242 2>/dev/null; do sleep 1; done",
    "ps aux | grep foo | wc -l",
    "xargs echo",
    "timeout 5 sleep 10",
    "echo $((1+2))",
    "echo ${HOME} > out.txt 2>&1",
]


@pytest.mark.parametrize("command", DENIED)
def test_guard_denies(command):
    assert guard.deny_reason(command), command


@pytest.mark.parametrize("command", ALLOWED)
def test_guard_allows(command):
    assert guard.deny_reason(command) is None, command


def test_guard_stops_runaway_nesting():
    command = "pkill x"
    for _ in range(12):
        command = "bash -c " + json.dumps(command)
    assert guard.deny_reason(command)


def run_hook(payload: object) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPTS / "lane_safety_guard.py")],
        input=payload if isinstance(payload, str) else json.dumps(payload),
        text=True, capture_output=True, check=False,
    )


def test_hook_emits_deny_decision_pointing_at_reap():
    done = run_hook({"tool_name": "Bash", "tool_input": {"command": "pkill node"}})
    assert done.returncode == 0
    output = json.loads(done.stdout)["hookSpecificOutput"]
    assert output["hookEventName"] == "PreToolUse"
    assert output["permissionDecision"] == "deny"
    assert "scripts/reap.py" in output["permissionDecisionReason"]


@pytest.mark.parametrize("payload", [
    {"tool_name": "Bash", "tool_input": {"command": "ls"}},
    {"tool_name": "Read", "tool_input": {"command": "pkill node"}},
    {"tool_name": "Bash", "tool_input": {}},
    {"tool_name": "Bash", "tool_input": {"command": 5}},
    "not json",
    "[]",
])
def test_hook_passes_everything_else(payload):
    done = run_hook(payload)
    assert done.returncode == 0
    assert done.stdout == ""


# reap


@pytest.fixture
def sleeper():
    processes = []

    def start(*arguments):
        process = subprocess.Popen(arguments or ("sleep", "60"))
        processes.append(process)
        time.sleep(0.2)
        return process

    yield start
    for process in processes:
        process.kill()
        process.wait()


def test_reap_stops_explicit_pid_and_reports_its_state(sleeper, capsys):
    process = sleeper()
    status = reap.main([str(process.pid), "--grace", "3"])
    out = capsys.readouterr().out
    assert status == 0
    assert process.wait(timeout=5) == -15
    assert f"pid {process.pid} age " in out
    assert f"parent {os.getpid()}" in out
    assert "cmd sleep 60" in out
    assert f"stopped pid {process.pid}" in out


def test_reap_escalates_to_kill_after_grace(sleeper, capsys):
    process = sleeper("sh", "-c", "trap '' TERM; while :; do sleep 1; done")
    status = reap.main([str(process.pid), "--grace", "0.3"])
    assert status == 0
    assert process.wait(timeout=5) == -9
    assert "(KILL)" in capsys.readouterr().out


def test_reap_refuses_process_newer_than_started_before(sleeper, capsys):
    process = sleeper()
    status = reap.main([str(process.pid), "--started-before", str(time.time() - 3600)])
    assert status == 1
    assert process.poll() is None
    assert "started after --started-before" in capsys.readouterr().out


def test_reap_accepts_process_older_than_started_before(sleeper):
    process = sleeper()
    cutoff = time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(time.time() + 5))
    assert reap.main([str(process.pid), "--started-before", cutoff, "--grace", "3"]) == 0
    process.wait(timeout=5)


def test_reap_tree_stops_descendants(sleeper):
    process = sleeper("sh", "-c", "sleep 60 & sleep 60 & wait")
    time.sleep(0.3)
    children = reap.descendants(process.pid)
    assert len(children) == 2
    assert reap.main([str(process.pid), "--tree", "--grace", "3"]) == 0
    process.wait(timeout=5)
    time.sleep(0.2)
    assert not any(reap.is_alive(child) for child in children)


def test_reap_without_tree_leaves_descendants(sleeper):
    process = sleeper("sh", "-c", "sleep 60 & wait")
    time.sleep(0.3)
    children = reap.descendants(process.pid)
    assert reap.main([str(process.pid), "--grace", "3"]) == 0
    process.wait(timeout=5)
    try:
        assert all(reap.is_alive(child) for child in children)
    finally:
        for child in children:
            os.kill(child, 9)


def test_reap_refuses_self_parent_and_init(capsys):
    for pid in (os.getpid(), os.getppid(), 1):
        assert reap.main([str(pid)]) == 1
    assert capsys.readouterr().out.count("refused pid") == 3


def test_reap_reports_missing_pid_as_gone(sleeper, capsys):
    process = sleeper("true")
    process.wait()
    assert reap.main([str(process.pid)]) == 0
    assert "already gone" in capsys.readouterr().out


@pytest.mark.parametrize("arguments", [
    ["-1"], ["12abc"], ["$(pgrep x)"], ["node"], ["1 2"],
    ["123", "--grace", "-1"], ["123", "--started-before", "yesterday"],
])
def test_reap_rejects_non_pid_arguments(arguments, capsys):
    with pytest.raises(SystemExit) as raised:
        status = reap.main(arguments)
        raise SystemExit(status)
    assert raised.value.code == 2


def test_reap_describes_age_compactly():
    assert reap.format_age(5) == "0m05s"
    assert reap.format_age(3725) == "1h02m"
    assert reap.format_age(90000) == "1d01h"


# pre-push guard

HOOK = SCRIPTS / "hooks" / "pre-push"
MARK = "<" * 7


def git(cwd, *arguments, check=True):
    environment = {
        key: value for key, value in os.environ.items() if not key.startswith("GIT_")
    }
    environment.update({
        "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_SYSTEM": os.devnull,
        "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
        "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
    })
    return subprocess.run(
        ["git", *arguments], cwd=cwd, env=environment, text=True,
        capture_output=True, check=check,
    )


def commit(repo, name, text="x\n"):
    (repo / name).write_text(text)
    git(repo, "add", name)
    git(repo, "commit", "-q", "-m", f"add {name}")


@pytest.fixture
def pushing(tmp_path):
    remote = tmp_path / "remote.git"
    git(tmp_path, "init", "-q", "--bare", "-b", "main", str(remote))
    seed = tmp_path / "seed"
    git(tmp_path, "init", "-q", "-b", "main", str(seed))
    commit(seed, "a.txt")
    git(seed, "push", "-q", str(remote), "main")
    clone = tmp_path / "clone"
    git(tmp_path, "clone", "-q", str(remote), str(clone))
    hook = clone / ".git" / "hooks" / "pre-push"
    hook.symlink_to(HOOK)
    return clone


def test_pre_push_allows_topic_branch_and_fast_forward_main(pushing):
    git(pushing, "switch", "-q", "-c", "topic")
    commit(pushing, "b.txt")
    assert git(pushing, "push", "-q", "origin", "topic", check=False).returncode == 0
    git(pushing, "switch", "-q", "main")
    commit(pushing, "c.txt")
    assert git(pushing, "push", "-q", "origin", "main", check=False).returncode == 0


def test_pre_push_refuses_default_branch_from_linked_worktree(pushing, tmp_path):
    worktree = tmp_path / "linked"
    git(pushing, "worktree", "add", "-q", "-b", "topic", str(worktree))
    commit(worktree, "b.txt")
    done = git(worktree, "push", "origin", "HEAD:main", check=False)
    assert done.returncode != 0
    assert "linked worktree" in done.stderr
    assert git(worktree, "push", "-q", "origin", "topic", check=False).returncode == 0


def test_pre_push_refuses_non_fast_forward_default_branch(pushing):
    commit(pushing, "b.txt")
    git(pushing, "push", "-q", "origin", "main")
    git(pushing, "reset", "-q", "--hard", "HEAD~1")
    commit(pushing, "c.txt")
    done = git(pushing, "push", "--force", "origin", "main", check=False)
    assert done.returncode != 0
    assert "non-fast-forward" in done.stderr


def test_pre_push_refuses_deleting_default_branch(pushing):
    done = git(pushing, "push", "origin", "--delete", "main", check=False)
    assert done.returncode != 0
    assert "deleting the default branch main" in done.stderr


def test_pre_push_allows_deleting_other_branches(pushing):
    git(pushing, "push", "-q", "origin", "main:scratch")
    assert git(pushing, "push", "-q", "origin", "--delete", "scratch", check=False).returncode == 0


def test_pre_push_refuses_conflict_markers_in_new_commits(pushing):
    git(pushing, "switch", "-q", "-c", "topic")
    commit(pushing, "b.txt", f"ok\n{MARK} HEAD\nmine\n")
    done = git(pushing, "push", "origin", "topic", check=False)
    assert done.returncode != 0
    assert "conflict markers" in done.stderr
    commit(pushing, "b.txt", "fixed\n")
    # the earlier commit is still in the range being pushed
    assert git(pushing, "push", "origin", "topic", check=False).returncode != 0


def test_pre_push_refuses_conflict_markers_left_in_a_merge(pushing):
    git(pushing, "switch", "-q", "-c", "topic")
    commit(pushing, "b.txt", "one\n")
    git(pushing, "switch", "-q", "main")
    commit(pushing, "c.txt", "two\n")
    git(pushing, "merge", "-q", "--no-commit", "--no-ff", "topic")
    (pushing / "m.txt").write_text(f"{MARK} HEAD\nleft behind\n")
    git(pushing, "add", "m.txt")
    git(pushing, "commit", "-q", "-m", "merge")
    done = git(pushing, "push", "origin", "main", check=False)
    assert done.returncode != 0
    assert "conflict markers" in done.stderr


def test_pre_push_ignores_markers_already_on_the_remote(pushing):
    commit(pushing, "b.txt", f"{MARK} HEAD\n")
    git(pushing, "push", "-q", "--no-verify", "origin", "main")
    commit(pushing, "c.txt")
    assert git(pushing, "push", "-q", "origin", "main", check=False).returncode == 0


def test_pre_push_ignores_bare_separator_lines(pushing):
    git(pushing, "switch", "-q", "-c", "topic")
    commit(pushing, "b.txt", "Title\n=======\n")
    assert git(pushing, "push", "-q", "origin", "topic", check=False).returncode == 0


# Claude settings


def guard_path(tmp_path):
    return tmp_path / "scripts" / "lane_safety_guard.py"


def test_settings_install_is_idempotent_and_preserves_other_hooks(tmp_path):
    settings = tmp_path / "settings.json"
    settings.write_text(json.dumps({
        "model": "x",
        "hooks": {"PreToolUse": [{"matcher": "Edit", "hooks": [{"type": "command", "command": "keep"}]}],
                  "Stop": [{"hooks": []}]},
    }))
    assert hooks.apply(settings, guard_path(tmp_path)) == "installed"
    assert hooks.apply(settings, guard_path(tmp_path)) == "unchanged"
    data = json.loads(settings.read_text())
    entries = data["hooks"]["PreToolUse"]
    assert [entry["matcher"] for entry in entries] == ["Edit", "Bash"]
    assert "lane_safety_guard.py" in entries[1]["hooks"][0]["command"]
    assert data["model"] == "x" and "Stop" in data["hooks"]


def test_settings_command_quotes_the_path_for_the_shell(tmp_path):
    guard_file = tmp_path / "a$(touch pwned)b" / "lane_safety_guard.py"
    assert hooks.hook_command(guard_file).startswith("python3 ")
    command = hooks.hook_command(guard_file, sys.executable)
    done = subprocess.run(
        ["sh", "-c", f"set -- {command}; printf '%s\\n' \"$1\" \"$2\""],
        capture_output=True, text=True, cwd=tmp_path, check=False,
    )
    assert done.stdout.splitlines() == [sys.executable, str(guard_file)]
    assert not (tmp_path / "pwned").exists()


def test_reap_skips_kill_when_pid_identity_changed(sleeper, monkeypatch, capsys):
    process = sleeper("sh", "-c", "trap '' TERM; while :; do sleep 1; done")
    real = reap.process_info
    calls = {"n": 0}

    def shifted(pid):
        info = real(pid)
        calls["n"] += pid == process.pid
        if info is not None and pid == process.pid and calls["n"] > 1:
            info["started"] = float(info["started"]) + 100
        return info

    monkeypatch.setattr(reap, "process_info", shifted)
    status = reap.reap([process.pid], grace=0.3)
    assert status == 1
    assert process.poll() is None
    assert "KILL skipped" in capsys.readouterr().out


def test_settings_repoint_replaces_the_managed_entry(tmp_path):
    settings = tmp_path / "settings.json"
    hooks.apply(settings, tmp_path / "zz-previous" / "lane_safety_guard.py")
    hooks.apply(settings, guard_path(tmp_path))
    entries = json.loads(settings.read_text())["hooks"]["PreToolUse"]
    assert len(entries) == 1 and "zz-previous" not in entries[0]["hooks"][0]["command"]


def test_settings_remove_leaves_only_foreign_entries(tmp_path):
    settings = tmp_path / "settings.json"
    hooks.apply(settings, guard_path(tmp_path))
    assert hooks.apply(settings, None) == "removed"
    assert json.loads(settings.read_text()) == {}


def test_settings_write_goes_through_a_symlink(tmp_path):
    real = tmp_path / "real.json"
    real.write_text("{}")
    link = tmp_path / "settings.json"
    link.symlink_to(real)
    hooks.apply(link, guard_path(tmp_path))
    assert link.is_symlink()
    assert "PreToolUse" in json.loads(real.read_text())["hooks"]


def test_settings_refuse_unreadable_json(tmp_path):
    settings = tmp_path / "settings.json"
    settings.write_text("{not json")
    with pytest.raises(hooks.ConfigError):
        hooks.apply(settings, guard_path(tmp_path))
    assert settings.read_text() == "{not json"
