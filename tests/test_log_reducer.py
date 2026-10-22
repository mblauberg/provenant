from __future__ import annotations

import hashlib
import importlib.util
import json
import os
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


def test_reducer_returns_original_for_unsupported_command():
    plain = "routine output\n" * 500
    unsupported = log_reducer.reduce_text(plain, command="make", exit_status=0)

    assert unsupported["text"] == plain
    assert unsupported["reduced"] is False


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


def test_ansi_coloured_failure_is_retained():
    source = ("routine output " + "x" * 80 + "\n") * 100 + "\x1b[31mFAILED tests/test_sample.py::test_bad\x1b[0m\n"

    result = log_reducer.reduce_text(source, command="pytest", exit_status=1)

    assert result["reduced"] is True
    assert "\x1b[31mFAILED tests/test_sample.py::test_bad\x1b[0m" in result["text"]


def test_failure_block_keeps_indented_assertion_traceback_and_context():
    block = """________________ test_bad __________________
source context: value = 1
        assert value == 2
E       assert 1 == 2
E        +  where 1 = value
E       Expected: 2
E        Received: 1
Traceback (most recent call last):
  File \"test_sample.py\", line 4
AssertionError: mismatch
"""
    source = ("routine output " + "x" * 80 + "\n") * 60 + block + ("routine tail " + "y" * 80 + "\n") * 40

    result = log_reducer.reduce_text(source, command="pytest", exit_status=1)

    assert result["reduced"] is True
    for line in block.splitlines():
        assert line in result["text"]


def test_quote_verifier_requires_exact_source_lines_in_order():
    verify = log_reducer._quotes_are_ordered_source_lines

    assert verify("prefix evidence suffix\nevidence\n", ["evidence"])
    assert not verify("prefix evidence suffix\n", ["evidence"])
    assert not verify("second\nfirst\n", ["first", "second"])


def test_cli_defaults_archive_under_instance_root(tmp_path):
    instance = tmp_path / "instance"
    source = ("normal output " + "x" * 90 + "\n") * 50 + "2 failed, 8 passed\n"
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--command", "pytest", "--exit-status", "1", "--pilot"],
        input=source.encode(), capture_output=True,
        env={**os.environ, "AGENT_FABRIC_INSTANCE_ROOT": str(instance)},
    )

    assert result.returncode == 0, result.stderr.decode()
    archives = list((instance / "state" / "log-reducer").glob("*.log"))
    assert len(archives) == 1
    assert archives[0].read_bytes() == source.encode()


def test_cli_refuses_to_archive_secret_scan_finding(tmp_path):
    instance = tmp_path / "instance"
    archive = tmp_path / "explicit.log"
    source = ("routine output " + "x" * 90 + "\n") * 50 + "xoxb-1234567890-1234567890\n"
    result = subprocess.run(
        [sys.executable, str(SCRIPT), "--command", "pytest", "--exit-status", "1", "--archive", str(archive)],
        input=source.encode(), capture_output=True,
        env={**os.environ, "AGENT_FABRIC_INSTANCE_ROOT": str(instance)},
    )

    assert result.returncode != 0
    assert b"secret" in result.stderr.lower()
    assert not archive.exists()


def test_interleaved_xdist_failures_keep_both_evidence_blocks():
    first = "FAILED tests/test_a.py::test_a\nE   assert 1 == 2\nE    +  where 1 = value\n"
    second = "FAILED tests/test_b.py::test_b\nE   Expected: 4\nE   Received: 3\n"
    source = ("routine output " + "x" * 80 + "\n") * 60 + first + "worker gw1 heartbeat\n" * 8 + second + ("routine tail " + "y" * 80 + "\n") * 25

    result = log_reducer.reduce_text(source, command="pytest", exit_status=1)

    assert result["reduced"] is True
    for block in (first, second):
        assert block in result["text"]


def test_unknown_format_falls_back_to_original():
    source = "ordinary compiler chatter\n" * 180 + "last opaque diagnostic\n"

    result = log_reducer.reduce_text(source, command="tsc", exit_status=1)

    assert result["reduced"] is False
    assert result["text"] == source


def test_typescript_error_keeps_indented_continuations():
    block = "src/example.ts(2,3): error TS2345: type mismatch\n  Related information:\n    required: string\n    found: number\n"
    source = ("routine output " + "x" * 90 + "\n") * 50 + block + ("routine tail " + "y" * 90 + "\n") * 30

    result = log_reducer.reduce_text(source, command="tsc", exit_status=2)

    assert result["reduced"] is True
    assert block.rstrip() in result["text"]


def test_vitest_expected_received_block_is_retained():
    block = """ FAIL  test/sample.test.ts > sample
AssertionError: values differ

- Expected
+ Received
  Object {
-   count: 2,
+   count: 1,
  }

 ❯ test/sample.test.ts:14:9
"""
    source = ("routine output " + "x" * 90 + "\n") * 50 + block + ("routine tail " + "y" * 90 + "\n") * 30

    result = log_reducer.reduce_text(source, command="vitest", exit_status=1)

    assert result["reduced"] is True
    assert block.rstrip() in result["text"]


def test_pytest_failure_keeps_source_context_through_section_end():
    block = """________________ test_bad __________________

    def test_bad():
        value = 1

        assert value == 2
E       assert 1 == 2

tests/test_sample.py:5: AssertionError
"""
    source = ("routine output " + "x" * 80 + "\n") * 60 + block + (
        "routine tail " + "y" * 80 + "\n"
    ) * 40

    result = log_reducer.reduce_text(source, command="pytest", exit_status=1)

    assert result["reduced"] is True
    assert block.rstrip() in result["text"]


def test_evidence_blocks_do_not_swallow_unrelated_output():
    failure = """_________________________________ test_x __________________________________

    def test_x():
>       assert compute() == 2
E       assert 1 == 2

tests/test_sample.py:5: AssertionError
"""
    tail = ("routine tail " + "y" * 90 + "\n") * 200
    passing = "============ 412 passed in 9.31s ============\n\n" + ("    detail " + "z" * 90 + "\n\n") * 200

    failed = log_reducer.reduce_text(failure + tail, command="pytest", exit_status=1)
    passed = log_reducer.reduce_text(passing, command="pytest", exit_status=0)

    assert failure.rstrip() in failed["text"]
    assert "routine tail" not in failed["text"]
    assert "412 passed" in passed["text"]
    assert "detail" not in passed["text"]


def test_gh_log_prefix_and_tsc_snippets_keep_their_continuations():
    stamp = "Python checks\tRun tests\t2026-10-22T08:09:36.7557617Z "
    traceback = [
        "Traceback (most recent call last):",
        '  File "a.py", line 3, in g',
        "    f()",
        "KeyError: 'x'",
    ]
    gh_log = "".join(stamp + line + "\n" for line in traceback)
    tsc = "src/a.ts:3:5 - error TS2322: Type 'number' is not assignable.\n\n3 foo(1);\n      ~\n"
    padding = ("routine output " + "x" * 90 + "\n") * 60

    gh_result = log_reducer.reduce_text(padding + gh_log, command="gh run view --log-failed", exit_status=1)
    tsc_result = log_reducer.reduce_text(padding + tsc, command="tsc", exit_status=2)

    assert gh_log in gh_result["text"]
    assert tsc in tsc_result["text"]
