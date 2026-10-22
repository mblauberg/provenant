from __future__ import annotations

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills/orchestrate/scripts/log_reducer.py"
SPEC = importlib.util.spec_from_file_location("log_reducer", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
log_reducer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(log_reducer)


def test_reducer_keeps_failure_error_counts_and_summary_and_marks_dropped_spans():
    lines = [f"routine output {index}: {'x' * 80}" for index in range(45)]
    lines[4] = "FAILED tests/test_sample.py::test_bad - AssertionError"
    lines[13] = "ERROR: fixture setup failed"
    lines[21] = "ValueError: invalid fixture input"
    lines[29] = "3 failed, 42 passed in 1.25s"
    lines[-1] = "FINAL SUMMARY: 3 failures"
    source = "\n".join(lines) + "\n"

    result = log_reducer.reduce_text(source, command="pytest", exit_status=1)

    assert result["reduced"] is True
    assert result["source_sha256"] == hashlib.sha256(source.encode()).hexdigest()
    assert result["exit_status"] == 1
    rendered = result["text"]
    for evidence in (lines[4], lines[13], lines[21], lines[29], lines[-1]):
        assert evidence in rendered
    assert "dropped" in rendered
    assert len(rendered.encode()) < len(source.encode())


def test_reducer_returns_original_for_unsupported_command_and_possible_credentials():
    plain = "routine output\n" * 500
    unsupported = log_reducer.reduce_text(plain, command="make", exit_status=0)
    credentialed = log_reducer.reduce_text(
        plain + "TOKEN=ghp_abcdefghijklmnopqrstuvwxyz123456\n",
        command="pytest",
        exit_status=0,
    )

    assert unsupported["text"] == plain
    assert unsupported["reduced"] is False
    assert credentialed["reduced"] is False
    assert credentialed["text"].endswith("TOKEN=ghp_abcdefghijklmnopqrstuvwxyz123456\n")


def test_cli_archives_full_source_and_writes_receipt(tmp_path):
    source = (("normal output " + "x" * 90 + "\n") * 50 + "2 failed, 8 passed\n").replace("\n", "\r\n")
    archive = tmp_path / "full.log"
    receipt = tmp_path / "receipt.json"
    result = subprocess.run(
        [
            sys.executable, str(SCRIPT), "--command", "pytest", "--exit-status", "1",
            "--archive", str(archive), "--receipt", str(receipt), "--pilot",
        ],
        input=source.encode(),
        capture_output=True,
        check=True,
    )

    assert archive.read_bytes() == source.encode()
    metadata = json.loads(receipt.read_text())
    assert metadata["source_sha256"] == hashlib.sha256(source.encode()).hexdigest()
    assert metadata["exit_status"] == 1
    assert metadata["reduced"] is True
    assert len(result.stdout) < len(source.encode())
