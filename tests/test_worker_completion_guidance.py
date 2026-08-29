from pathlib import Path
import re
import signal
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]


GUIDANCE_FILES = (
    ROOT / "skills/orchestrate/references/worker-liveness.md",
    ROOT / "agents/codex-analyst.md",
    ROOT / "agents/codex-implementer.md",
)
DETACHED_HELPER = ROOT / "skills/orchestrate/scripts/run_worker_detached.sh"


def _active_fifo_completion_patterns() -> tuple[re.Pattern[str], ...]:
    return (
        re.compile(r"\bmkfifo\b", re.IGNORECASE),
        re.compile(r"\b(?:done|codex-[^\s`]+)\.fifo\b", re.IGNORECASE),
        re.compile(r"\bcat\s+[^\n]*\.fifo\b", re.IGNORECASE),
    )


def test_worker_completion_guidance_does_not_use_fifo_rendezvous():
    source = GUIDANCE_FILES[0].read_text()

    for pattern in _active_fifo_completion_patterns():
        assert not pattern.search(source), pattern.pattern

    assert "run the worker in the foreground" in source
    assert 'wait "$WRAPPER_PID"' in source
    assert "regular completion file" in source
    assert "foreground wait observes\nthat PID's exit" in source
    assert "Codex-only" in source
    assert "agy, cursor, kiro" in source


def test_codex_agent_definitions_use_foreground_or_durable_pid_completion():
    for path in GUIDANCE_FILES[1:]:
        source = path.read_text()
        for pattern in _active_fifo_completion_patterns():
            assert not pattern.search(source), f"{path}: {pattern.pattern}"
        assert "foreground" in source.lower(), path
        assert 'wait "$PID"' in source or "regular completion file" in source, path


def test_codex_liveness_does_not_infer_exit_from_resource_signals():
    for path in GUIDANCE_FILES[1:]:
        source = path.read_text()
        normalised = " ".join(source.split())
        assert "worker-liveness.md" in source, path
        assert "CPU time, elapsed time or output file size do not prove exit" in normalised, path
        assert "observed PID exit and its exit status" in normalised, path
        assert not re.search(
            r"minutes?\s+elapsed.*near-zero\s+CPU.*(?:means|is)\s+(?:hung|dead)",
            source,
            re.IGNORECASE | re.DOTALL,
        ), path


def test_detached_protocol_uses_owned_run_and_pids():
    source = GUIDANCE_FILES[0].read_text()
    assert "run_worker_detached.sh" in source
    assert "--run-dir" in source
    assert "--transcript" not in source
    assert "worker.pid" in source
    assert "wrapper.pid" in source
    assert 'terminal-report --pid "$WORKER_PID"' in source
    assert 'terminal-report --pid "$PID"' not in source

    for path in GUIDANCE_FILES[1:]:
        agent_source = path.read_text()
        assert "run_worker_detached.sh" in agent_source, path
        assert "--transcript" not in agent_source, path
        assert "worker.pid" in agent_source, path
        assert "wrapper.pid" in agent_source, path


def _helper_command(run_dir, worker_code):
    return [
        str(DETACHED_HELPER),
        "--run-dir",
        str(run_dir),
        "--",
        sys.executable,
        "-c",
        worker_code,
    ]


def test_detached_helper_records_child_and_validates_reentry(tmp_path):
    worker_code = "import time, sys; time.sleep(0.15); print('worker'); sys.exit(7)"
    run_dir = tmp_path / "run"
    process = subprocess.run(
        _helper_command(run_dir, worker_code),
        capture_output=True,
        text=True,
    )

    assert process.returncode == 7
    assert (run_dir / "transcript.txt").read_text().strip() == "worker"
    marker = dict(line.split("=", 1) for line in (run_dir / "done").read_text().splitlines())
    assert marker["exit"] == "7"
    assert marker["worker_pid"] == (run_dir / "worker.pid").read_text().strip()
    assert marker["wrapper_pid"] == (run_dir / "wrapper.pid").read_text().strip()
    assert marker["worker_pid"] != marker["wrapper_pid"]

    validation = subprocess.run(
        [str(DETACHED_HELPER), "--validate", "--run-dir", str(run_dir)],
        capture_output=True,
        text=True,
    )
    assert validation.returncode == 0
    worker_pid, wrapper_pid, status = validation.stdout.split()
    assert (worker_pid, wrapper_pid, status) == (
        marker["worker_pid"], marker["wrapper_pid"], marker["exit"]
    )


def test_detached_helper_same_run_dir_allows_only_one_provider(tmp_path):
    run_dir = tmp_path / "same"
    launches = tmp_path / "launches"
    worker_code = (
        "import sys, time; "
        "open(sys.argv[1], 'a').write('x'); "
        "time.sleep(.15); sys.exit(7)"
    )
    processes = [
        subprocess.Popen(
            _helper_command(run_dir, worker_code) + [str(launches)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        for _ in (1, 2)
    ]
    statuses = [process.wait(timeout=5) for process in processes]

    assert sorted(statuses) == [2, 7]
    assert launches.read_text() == "x"
    marker = dict(line.split("=", 1) for line in (run_dir / "done").read_text().splitlines())
    assert marker["exit"] == "7"


def test_detached_helper_forwards_stdin_to_provider(tmp_path):
    run_dir = tmp_path / "stdin"
    worker_code = "import sys; value = sys.stdin.read(); print(value, end=''); sys.exit(0)"
    result = subprocess.run(
        _helper_command(run_dir, worker_code),
        input="brief from stdin\n",
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0
    assert (run_dir / "transcript.txt").read_text() == "brief from stdin\n"


def test_detached_helper_propagates_signal_exit(tmp_path):
    run_dir = tmp_path / "signal"
    worker_code = "import os, signal; os.kill(os.getpid(), signal.SIGTERM)"
    result = subprocess.run(_helper_command(run_dir, worker_code), capture_output=True, text=True)

    assert result.returncode == 128 + signal.SIGTERM
    marker = dict(line.split("=", 1) for line in (run_dir / "done").read_text().splitlines())
    assert marker["exit"] == str(128 + signal.SIGTERM)


def test_detached_helper_rejects_invalid_setup_before_launch(tmp_path):
    blocked_parent = tmp_path / "not-a-directory"
    blocked_parent.write_text("block")
    launched = tmp_path / "launched"
    worker_code = "import sys; from pathlib import Path; Path(sys.argv[1]).write_text('launched')"
    result = subprocess.run(
        _helper_command(blocked_parent / "invalid", worker_code)
        + [str(launched)],
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert not launched.exists()
    assert not (blocked_parent / "invalid").exists()


def test_detached_helper_validation_reports_startup(tmp_path):
    run_dir = tmp_path / "starting"
    run_dir.mkdir()
    wrapper = subprocess.Popen(["sleep", "1"])
    (run_dir / "wrapper.pid").write_text(f"{wrapper.pid}\n")
    result = subprocess.run(
        [str(DETACHED_HELPER), "--validate", "--run-dir", str(run_dir)],
        capture_output=True,
        text=True,
    )
    wrapper.wait(timeout=5)

    assert result.returncode == 1


def test_detached_helper_validation_rejects_malformed_marker(tmp_path):
    run_dir = tmp_path / "malformed"
    run_dir.mkdir()
    (run_dir / "wrapper.pid").write_text("999999\n")
    (run_dir / "worker.pid").write_text("888888\n")
    (run_dir / "done").write_text("wrapper_pid=not-numeric\nworker_pid=888888\nexit=wat\n")
    validation = subprocess.run(
        [str(DETACHED_HELPER), "--validate", "--run-dir", str(run_dir)],
        capture_output=True,
        text=True,
    )

    assert validation.returncode != 0
    assert "marker" in validation.stderr


def test_reentry_guidance_fails_when_wrapper_exits_without_marker():
    source = GUIDANCE_FILES[0].read_text()
    normalised = " ".join(source.split())
    assert "completion evidence missing" in normalised
    assert "--validate" in source
    assert "kill -0 \"$WRAPPER_PID\"" not in source
    assert "recorded wrapper exit" in normalised
