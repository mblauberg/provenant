from pathlib import Path
import signal
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]


DETACHED_HELPER = ROOT / "skills/orchestrate/scripts/run_worker_detached.sh"


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
    stale = subprocess.run(
        [str(DETACHED_HELPER), "--validate", "--run-dir", str(run_dir)],
        capture_output=True,
        text=True,
    )
    assert stale.returncode == 2


def test_detached_helper_validation_graces_fresh_claim_without_wrapper_pid(tmp_path):
    run_dir = tmp_path / "claimed"
    run_dir.mkdir()
    fresh = subprocess.run(
        [str(DETACHED_HELPER), "--validate", "--run-dir", str(run_dir)],
        capture_output=True,
        text=True,
    )
    time.sleep(3)
    stale = subprocess.run(
        [str(DETACHED_HELPER), "--validate", "--run-dir", str(run_dir)],
        capture_output=True,
        text=True,
    )

    assert fresh.returncode == 1
    assert stale.returncode == 2


def test_detached_helper_validation_rejects_malformed_marker(tmp_path):
    run_dir = tmp_path / "malformed"
    run_dir.mkdir()
    (run_dir / "wrapper.pid").write_text("999999\n")
    (run_dir / "worker.pid").write_text("888888\n")
    (run_dir / "done").write_text("wrapper_pid=999999\nworker_pid=888888\nexit=wat\n")
    validation = subprocess.run(
        [str(DETACHED_HELPER), "--validate", "--run-dir", str(run_dir)],
        capture_output=True,
        text=True,
    )

    assert validation.returncode != 0
    assert "marker" in validation.stderr
