from pathlib import Path
import os
import re
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


def test_detached_protocol_names_both_pids_and_uses_unique_run_paths():
    source = GUIDANCE_FILES[0].read_text()
    assert "run_worker_detached.sh" in source
    assert "--run-dir" in source
    assert "--transcript" in source
    assert "worker.pid" in source
    assert "wrapper.pid" in source
    assert 'terminal-report --pid "$WORKER_PID"' in source
    assert 'terminal-report --pid "$PID"' not in source

    for path in GUIDANCE_FILES[1:]:
        agent_source = path.read_text()
        assert "run_worker_detached.sh" in agent_source, path
        assert "worker.pid" in agent_source, path
        assert "wrapper.pid" in agent_source, path


def _helper_command(run_dir, transcript, worker_code):
    return [
        str(DETACHED_HELPER),
        "--run-dir",
        str(run_dir),
        "--transcript",
        str(transcript),
        "--",
        sys.executable,
        "-c",
        worker_code,
    ]


def test_detached_helper_binds_child_pid_and_isolates_fresh_markers(tmp_path):
    """Exercise the shared detached helper with two independently claimed runs."""
    worker_code = "import time, sys; time.sleep(0.15); print('worker'); sys.exit(7)"
    processes = [
        subprocess.Popen(
            _helper_command(tmp_path / name, tmp_path / name / "transcript", worker_code),
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        for name in ("run-a", "run-b")
    ]
    for process in processes:
        assert process.wait(timeout=5) == 7, process.stderr.read()

    markers = []
    for name in ("run-a", "run-b"):
        root = tmp_path / name
        marker = (root / "done").read_text()
        values = dict(line.split("=", 1) for line in marker.splitlines())
        assert values["exit"] == "7"
        assert values["worker_pid"] == (root / "worker.pid").read_text().strip()
        assert values["wrapper_pid"] == (root / "wrapper.pid").read_text().strip()
        assert values["wrapper_pid"] in {str(process.pid) for process in processes}
        assert values["worker_pid"] != values["wrapper_pid"]
        assert (root / "transcript").read_text().strip() == "worker"
        for pid in (int(values["worker_pid"]), int(values["wrapper_pid"])):
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                pass
            else:
                raise AssertionError(f"completion accepted live PID {pid}")
        markers.append(marker)

    assert markers[0] != markers[1]


def test_detached_helper_rejects_existing_run_dir_before_provider_launch(tmp_path):
    run_dir = tmp_path / "existing"
    run_dir.mkdir()
    (run_dir / "done").write_text("wrapper_pid=stale\nworker_pid=stale\nexit=99\n")
    launched = tmp_path / "launched"
    worker_code = "import sys; from pathlib import Path; Path(sys.argv[1]).write_text('launched')"
    result = subprocess.run(
        _helper_command(run_dir, tmp_path / "transcript", worker_code)
        + [str(launched)],
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert not launched.exists()
    assert (run_dir / "done").read_text().startswith("wrapper_pid=stale")
    assert "already exists" in result.stderr


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
            _helper_command(run_dir, tmp_path / f"transcript-{index}", worker_code)
            + [str(launches)],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            text=True,
        )
        for index in (1, 2)
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
        _helper_command(run_dir, tmp_path / "transcript", worker_code),
        input="brief from stdin\n",
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0
    assert (tmp_path / "transcript").read_text() == "brief from stdin\n"
    assert '<&0 &' in DETACHED_HELPER.read_text()


def test_detached_helper_rejects_invalid_transcript_setup_before_launch(tmp_path):
    blocked_parent = tmp_path / "not-a-directory"
    blocked_parent.write_text("block")
    launched = tmp_path / "launched"
    worker_code = "import sys; from pathlib import Path; Path(sys.argv[1]).write_text('launched')"
    result = subprocess.run(
        _helper_command(tmp_path / "invalid", blocked_parent / "transcript", worker_code)
        + [str(launched)],
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert not launched.exists()
    assert not (tmp_path / "invalid" / "done").exists()


def test_detached_helper_rejects_transcript_alias_of_evidence_before_launch(tmp_path):
    worker_code = "import sys; from pathlib import Path; Path(sys.argv[1]).write_text('launched')"
    for marker_name in ("done", "wrapper.identity", "wrapper.identity.tmp.fake"):
        run_dir = tmp_path / marker_name.replace(".", "-")
        launched = tmp_path / f"launched-{marker_name.replace('.', '-')}"
        result = subprocess.run(
            _helper_command(run_dir, run_dir / "missing" / ".." / marker_name, worker_code)
            + [str(launched)],
            capture_output=True,
            text=True,
        )

        assert result.returncode != 0
        assert not launched.exists()
        assert not (run_dir / marker_name).exists()
        assert "evidence" in result.stderr


def test_detached_helper_validation_rejects_reused_wrapper_identity(tmp_path):
    run_dir = tmp_path / "reused"
    run_dir.mkdir()
    (run_dir / "wrapper.pid").write_text(f"{os.getpid()}\n")
    (run_dir / "wrapper.identity").write_text("identity from a different process\n")
    (run_dir / "worker.pid").write_text("999999\n")
    (run_dir / "done").write_text(
        f"wrapper_pid={os.getpid()}\nworker_pid=999999\nexit=0\n"
    )
    result = subprocess.run(
        [str(DETACHED_HELPER), "--validate", "--run-dir", str(run_dir)],
        capture_output=True,
        text=True,
    )

    assert result.returncode != 0
    assert "identity" in result.stderr


def test_detached_helper_validation_rejects_malformed_marker(tmp_path):
    run_dir = tmp_path / "malformed"
    worker_code = "import sys; sys.exit(0)"
    result = subprocess.run(
        _helper_command(run_dir, tmp_path / "transcript", worker_code),
        capture_output=True,
        text=True,
    )
    assert result.returncode == 0
    (run_dir / "done").write_text(
        "wrapper_pid=not-numeric\nworker_pid=also-not-numeric\nexit=wat\n"
    )
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


def test_documented_validation_parsing_produces_numeric_pids(tmp_path):
    for path in GUIDANCE_FILES:
        source = path.read_text()
        assert 'WORKER_PID="${WORKER_PID#worker_pid=}"' in source
        assert 'WRAPPER_PID="${WRAPPER_PID#wrapper_pid=}"' in source
        assert 'STATUS="${STATUS#exit=}"' in source

    run_dir = tmp_path / "parse"
    result = subprocess.run(
        _helper_command(run_dir, tmp_path / "transcript", "import sys; sys.exit(7)"),
        capture_output=True,
        text=True,
    )
    assert result.returncode == 7
    script = f'''\
helper={DETACHED_HELPER!s}
run_dir={run_dir!s}
validation="$($helper --validate --run-dir "$run_dir")"
read -r WORKER_PID WRAPPER_PID STATUS <<< "$validation"
WORKER_PID="${{WORKER_PID#worker_pid=}}"
WRAPPER_PID="${{WRAPPER_PID#wrapper_pid=}}"
STATUS="${{STATUS#exit=}}"
printf '%s %s %s\\n' "$WORKER_PID" "$WRAPPER_PID" "$STATUS"
'''
    parsed = subprocess.run(["bash", "-c", script], capture_output=True, text=True)

    assert parsed.returncode == 0
    worker_pid, wrapper_pid, status = parsed.stdout.strip().split()
    assert worker_pid.isdigit()
    assert wrapper_pid.isdigit()
    assert status == "7"
