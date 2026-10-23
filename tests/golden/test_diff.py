"""Fast front-door differential smoke and allowlist contract."""

import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from run import binary_path  # noqa: E402


HERE = Path(__file__).resolve().parent


def test_differential_runner_same_changed_and_allowed(tmp_path):
    binary = binary_path()
    if binary is None or not binary.is_file():
        reason = "no CLI binary found (set PROVENANT_GOLDEN_BIN or build tools/front-door)"
        if os.environ.get("PROVENANT_GOLDEN_REQUIRED") == "1":
            pytest.fail(reason)
        pytest.skip(reason)
    fake = tmp_path / "fake-front-door"
    fake.write_text(
        '#!/bin/sh\nif [ "$1" = root ]; then printf "changed stdout\\n"; exit 0; fi\n'
        f'exec {shlex.quote(str(binary))} "$@"\n'
    )
    fake.chmod(0o755)

    def run(label, b, *extra):
        output = tmp_path / label
        completed = subprocess.run([
            sys.executable, str(HERE / "diff.py"), "--a", str(binary), "--b", str(b),
            "--case", "root-case-01", "--case", "help-case-01", "--jobs", "2",
            "--output-dir", str(output), *extra,
        ], capture_output=True, text=True, timeout=60)
        assert output.is_dir(), completed.stderr
        rows = [json.loads(line) for line in (output / "divergences.jsonl").read_text().splitlines()]
        return completed, rows, json.loads((output / "summary.json").read_text())

    # Reports inside the checkout must not trip guards in parallel children.
    with tempfile.TemporaryDirectory(prefix="golden-report-", dir=HERE) as directory:
        output = Path(directory) / "reports"
        local = subprocess.run([
            sys.executable, str(HERE / "diff.py"), "--a", str(binary), "--b", str(binary),
            "--case", "root-case-01", "--case", "help-case-01", "--jobs", "2",
            "--output-dir", str(output),
        ], capture_output=True, text=True, timeout=60)
        assert local.returncode == 0, local.stdout + local.stderr
        assert json.loads((output / "summary.json").read_text())["unexpected"] == 0

    same, rows, summary = run("same", binary)
    assert same.returncode == 0, same.stderr
    assert rows == []
    assert summary["cases"] == 2
    assert summary["divergences"] == 0

    changed, rows, summary = run("changed", fake)
    assert changed.returncode == 1, changed.stderr
    assert len(rows) == 1
    assert (rows[0]["case"], rows[0]["verb"], rows[0]["field"]) == ("root-case-01", "root", "stdout")
    assert rows[0]["allowed"] is False
    assert "changed stdout" in rows[0]["diff"]
    assert summary["per_verb"]["root"]["unexpected"] == 1

    allowlist = tmp_path / "allow.json"
    allowlist.write_text(json.dumps([
        {"case": "root-case-01", "field": "stdout", "reason": "Test fixture output", "b": "^changed stdout\\n$"},
        {"case": "nonexistent", "field": "stdout", "reason": "Stale entry for warning coverage"},
    ]))
    allowed, rows, summary = run("allowed", fake, "--allowlist", str(allowlist))
    assert allowed.returncode == 0, allowed.stderr
    assert len(rows) == 1 and rows[0]["allowed"] is True
    assert summary["allowed"] == 1 and summary["unexpected"] == 0
    assert len(summary["stale_allowlist"]) == 1
    assert "stale" in allowed.stderr

    # An a/b pattern must constrain the actual value, not merely case/field.
    allowlist.write_text(json.dumps([
        {"case": "root-case-01", "field": "stdout", "reason": "Wrong expected output", "b": "^other$"},
    ]))
    rejected, rows, _ = run("pattern-rejected", fake, "--allowlist", str(allowlist))
    assert rejected.returncode == 1
    assert rows[0]["allowed"] is False

    impl_binary = tmp_path / "impl-front-door"
    impl_binary.write_text('#!/bin/sh\nprintf "%s\\n" "$PROVENANT_LANE_IMPL"\n')
    impl_binary.chmod(0o755)
    impl_output = tmp_path / "impl"
    impl = subprocess.run([
        sys.executable, str(HERE / "diff.py"), "--a", str(impl_binary),
        "--impl-env", "PROVENANT_LANE_IMPL=python,rust", "--case", "root-case-01",
        "--output-dir", str(impl_output),
    ], capture_output=True, text=True, timeout=60)
    assert impl.returncode == 1, impl.stderr
    impl_rows = [json.loads(line) for line in (impl_output / "divergences.jsonl").read_text().splitlines()]
    assert len(impl_rows) == 1
    assert (impl_rows[0]["a"], impl_rows[0]["b"]) == ("python\n", "rust\n")

    blocked_output = tmp_path / "blocked-env"
    blocked = subprocess.run([
        sys.executable, str(HERE / "diff.py"), "--a", str(binary),
        "--impl-env", f"GH_CONFIG_DIR={tmp_path / 'outside-a'},{tmp_path / 'outside-b'}",
        "--case", "root-case-01", "--output-dir", str(blocked_output),
    ], capture_output=True, text=True, timeout=60)
    assert blocked.returncode == 1
    assert not (tmp_path / "outside-a").exists() and not (tmp_path / "outside-b").exists()
    assert json.loads((blocked_output / "divergences.jsonl").read_text())["field"] == "execution_error"

    failing = tmp_path / "failing-front-door"
    failing.write_text("#!/bin/sh\nexit 2\n")
    failing.chmod(0o755)
    allowlist.write_text(json.dumps([
        {"case": "worktree-list", "field": "execution_error", "reason": "Must never hide failed setup"},
    ]))
    failure_output = tmp_path / "failed-setup"
    failure = subprocess.run([
        sys.executable, str(HERE / "diff.py"), "--a", str(failing), "--b", str(failing),
        "--case", "worktree-list", "--allowlist", str(allowlist), "--output-dir", str(failure_output),
    ], capture_output=True, text=True, timeout=60)
    assert failure.returncode == 1, failure.stderr
    failures = [json.loads(line) for line in (failure_output / "divergences.jsonl").read_text().splitlines()]
    assert len(failures) == 1 and failures[0]["field"] == "execution_error"
    assert failures[0]["allowed"] is False


def test_unexpected_case_exception_is_reported(monkeypatch):
    import importlib.util
    spec = importlib.util.spec_from_file_location("golden_diff", HERE / "diff.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    def broken(*args, **kwargs):
        raise KeyError("unexpected case failure")
    monkeypatch.setattr(module, "run_case", broken)
    from run import Case
    case = Case("test", "root", ["root"], "machine")
    _, fields = module.compare_case(case, Path("a"), Path("b"), {}, {})
    assert len(fields) == 1 and fields[0][0] == "execution_error"
    assert "KeyError" in fields[0][1] and fields[0][1] == fields[0][2]
