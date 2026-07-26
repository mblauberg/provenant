import importlib.util
import json
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "orchestrate" / "scripts" / "worker_liveness.py"
SPEC = importlib.util.spec_from_file_location("worker_liveness", SCRIPT)
assert SPEC and SPEC.loader
worker_liveness = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = worker_liveness
SPEC.loader.exec_module(worker_liveness)


def test_duration_parsers_cover_ps_formats():
    assert worker_liveness.parse_duration("1:02:03") == 3723
    assert worker_liveness.parse_elapsed("2-01:02:03") == 176523


def test_stall_rule_uses_cpu_and_log_age_together(monkeypatch, tmp_path):
    monkeypatch.setattr(
        worker_liveness,
        "live_processes",
        lambda: [worker_liveness.Process(41, 1, "01:00", "0:00.32", "codex exec task")],
    )
    monkeypatch.setattr(worker_liveness, "process_cwd", lambda _pid: str(tmp_path))
    monkeypatch.setattr(worker_liveness, "worktree_state", lambda _cwd: "dirty")
    monkeypatch.setattr(
        worker_liveness,
        "session_logs",
        lambda _root: {str(tmp_path): tmp_path / "rollout-1.jsonl"},
    )
    log = tmp_path / "rollout-1.jsonl"
    log.write_text("{}\n")
    monkeypatch.setattr(worker_liveness, "log_metadata", lambda _path: ("2026-01-01T00:00:00+00:00", 3600.0))

    [worker] = worker_liveness.collect(tmp_path)

    assert worker.stalled is True
    assert worker.cpu == "0:00.32"
    assert worker.worktree == "dirty"


def test_short_healthy_worker_is_not_stalled(monkeypatch, tmp_path):
    monkeypatch.setattr(
        worker_liveness,
        "live_processes",
        lambda: [worker_liveness.Process(42, 1, "00:12", "0:01.21", "codex exec task")],
    )
    monkeypatch.setattr(worker_liveness, "process_cwd", lambda _pid: str(tmp_path))
    monkeypatch.setattr(worker_liveness, "worktree_state", lambda _cwd: "clean")
    monkeypatch.setattr(worker_liveness, "session_logs", lambda _root: {})

    [worker] = worker_liveness.collect(tmp_path)

    assert worker.stalled is False
    assert worker.session_log_mtime == "-"


def test_missing_session_log_does_not_clear_cpu_stall(monkeypatch, tmp_path):
    monkeypatch.setattr(
        worker_liveness,
        "live_processes",
        lambda: [worker_liveness.Process(43, 1, "45:00", "0:00.01", "codex exec task")],
    )
    monkeypatch.setattr(worker_liveness, "process_cwd", lambda _pid: str(tmp_path))
    monkeypatch.setattr(worker_liveness, "worktree_state", lambda _cwd: "clean")
    monkeypatch.setattr(worker_liveness, "session_logs", lambda _root: {})

    [worker] = worker_liveness.collect(tmp_path)

    assert worker.stalled is True
    assert worker.session_log_mtime == "-"


def test_live_processes_collapses_codex_exec_wrapper_and_child(monkeypatch):
    ps_output = """\
 7813     1 32:35 0:00.00 /bin/zsh -lc codex exec --cd /repo/.worktrees/impl-421 task
 7816  7813 32:35 0:20.16 /opt/homebrew/bin/codex exec --cd /repo/.worktrees/impl-421 task
"""
    monkeypatch.setattr(
        worker_liveness.subprocess,
        "run",
        lambda *_args, **_kwargs: subprocess.CompletedProcess([], 0, ps_output, ""),
    )

    processes = list(worker_liveness.live_processes())

    assert len(processes) == 1
    assert processes[0].pid == 7816
    assert processes[0].cpu == "0:20.16"


def test_collect_uses_codex_cd_target_instead_of_launch_cwd(monkeypatch, tmp_path):
    launch_cwd = tmp_path / "launcher"
    target_cwd = tmp_path / "repo" / ".worktrees" / "impl-421"
    monkeypatch.setattr(
        worker_liveness,
        "live_processes",
        lambda: [
            worker_liveness.Process(
                44, 1, "05:00", "0:10.00",
                f"/opt/homebrew/bin/codex exec --cd {target_cwd} task",
            ),
        ],
    )
    monkeypatch.setattr(worker_liveness, "process_cwd", lambda _pid: str(launch_cwd))
    monkeypatch.setattr(worker_liveness, "worktree_state", lambda _cwd: "clean")
    monkeypatch.setattr(worker_liveness, "session_logs", lambda _root: {})

    [worker] = worker_liveness.collect(tmp_path)

    assert worker.cwd == str(target_cwd)


def test_session_log_is_associated_with_normalized_cd_target(monkeypatch, tmp_path):
    sessions_root = tmp_path / "sessions"
    sessions_root.mkdir()
    target_cwd = tmp_path / "repo" / ".worktrees" / "impl-421"
    target_cwd.mkdir(parents=True)
    launch_cwd = tmp_path / "launcher"
    launch_cwd.mkdir()
    log = sessions_root / "rollout-target.jsonl"
    log.write_text(json.dumps({"payload": {"cwd": f"{target_cwd}/"}}) + "\n")
    launch_log = sessions_root / "rollout-launch.jsonl"
    launch_log.write_text(json.dumps({"payload": {"cwd": str(launch_cwd)}}) + "\n")
    monkeypatch.setattr(
        worker_liveness,
        "live_processes",
        lambda: [
            worker_liveness.Process(
                45, 1, "05:00", "0:10.00",
                f"/opt/homebrew/bin/codex exec --cd {target_cwd} task",
            ),
        ],
    )
    monkeypatch.setattr(worker_liveness, "process_cwd", lambda _pid: str(launch_cwd))
    monkeypatch.setattr(worker_liveness, "worktree_state", lambda _cwd: "clean")

    [worker] = worker_liveness.collect(sessions_root)

    assert worker.session_log == str(log)


def test_repo_option_excludes_workers_from_unrelated_repositories(
    monkeypatch, tmp_path, capsys,
):
    repo = tmp_path / "repo"
    target = repo / ".worktrees" / "impl-421"
    other_target = tmp_path / "seal-app" / ".worktrees" / "inventory"
    repo.mkdir()
    monkeypatch.setattr(
        worker_liveness,
        "live_processes",
        lambda: [
            worker_liveness.Process(
                46, 1, "05:00", "0:10.00",
                f"codex exec --cd {target} task",
            ),
            worker_liveness.Process(
                47, 1, "05:00", "0:10.00",
                f"codex exec --cd {other_target} task",
            ),
        ],
    )
    monkeypatch.setattr(worker_liveness, "process_cwd", lambda _pid: str(tmp_path))
    monkeypatch.setattr(worker_liveness, "session_logs", lambda _root: {})
    monkeypatch.setattr(worker_liveness, "worktree_state", lambda _cwd: "clean")
    monkeypatch.setattr(
        worker_liveness,
        "repository_id",
        lambda path: "repo" if str(path).startswith(str(repo)) else "other",
        raising=False,
    )

    result = worker_liveness.main([
        "--sessions-root", str(tmp_path),
        "--repo", str(repo),
        "--json",
    ])

    assert result == 0
    assert [row["pid"] for row in json.loads(capsys.readouterr().out)] == [46]


def test_repo_scope_defaults_to_current_repository(monkeypatch, tmp_path, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.chdir(repo)
    observed = {}

    def fake_collect(sessions_root, repo_scope):
        observed["sessions_root"] = sessions_root
        observed["repo_scope"] = repo_scope
        return []

    monkeypatch.setattr(worker_liveness, "collect", fake_collect)

    result = worker_liveness.main(["--sessions-root", str(tmp_path)])

    assert result == 0
    assert observed == {"sessions_root": tmp_path, "repo_scope": repo}
    assert capsys.readouterr().out.startswith("PID")


def test_process_cwd_uses_lsof_on_darwin_even_if_proc_path_appears_present(
    monkeypatch,
):
    calls = []

    def fake_run(command, **_kwargs):
        calls.append(command)
        return subprocess.CompletedProcess(command, 0, "p48\nn/repo/.worktrees/impl-421\n", "")

    monkeypatch.setattr(worker_liveness.sys, "platform", "darwin")
    monkeypatch.setattr(worker_liveness.Path, "exists", lambda _path: True)
    monkeypatch.setattr(worker_liveness.subprocess, "run", fake_run)

    cwd = worker_liveness.process_cwd(48)

    assert cwd == "/repo/.worktrees/impl-421"
    assert calls == [["lsof", "-a", "-p", "48", "-d", "cwd", "-Fn"]]


def test_worktree_state_disables_optional_git_locks(monkeypatch):
    calls = []

    def fake_run(command, **_kwargs):
        calls.append(command)
        return subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(worker_liveness.subprocess, "run", fake_run)

    state = worker_liveness.worktree_state("/repo/.worktrees/impl-421")

    assert state == "clean"
    assert calls == [[
        "git", "--no-optional-locks", "-C", "/repo/.worktrees/impl-421",
        "status", "--porcelain=v1", "--untracked-files=all",
    ]]
