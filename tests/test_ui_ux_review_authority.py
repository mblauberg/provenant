from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys

import pytest


ROOT = Path(__file__).resolve().parents[1]
SKILL = ROOT / "skills" / "ui-ux-design"
RUNNER_PATH = SKILL / "evals" / "review_boundary.py"


def _runner():
    previous = sys.dont_write_bytecode
    sys.dont_write_bytecode = True
    try:
        spec = importlib.util.spec_from_file_location("ui_ux_review_boundary", RUNNER_PATH)
        assert spec and spec.loader
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module
    finally:
        sys.dont_write_bytecode = previous


def _read_event() -> dict:
    return {"schema_version": 1, "channel": "filesystem", "effect": "read"}


def _run_boundary(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(RUNNER_PATH), *args],
        check=False,
        capture_output=True,
        text=True,
    )


def _write_json(path: Path, value: object) -> None:
    path.write_text(json.dumps(value))


def test_review_manifest_records_bytes_modes_and_symlinks_without_exclusions(tmp_path):
    runner = _runner()
    source = tmp_path / "source"
    source.mkdir()
    file = source / "screen.html"
    file.write_text("before")
    os.chmod(file, 0o640)
    (source / "screen-link").symlink_to("screen.html")

    manifest = runner.tree_manifest(source)

    assert manifest["schema_version"] == 2
    assert manifest["root"] == str(source.resolve())
    assert "excluded" not in manifest
    by_path = {entry["path"]: entry for entry in manifest["entries"]}
    assert by_path["screen.html"]["kind"] == "file"
    assert by_path["screen.html"]["mode"] == "0640"
    assert len(by_path["screen.html"]["sha256"]) == 64
    assert by_path["screen-link"]["kind"] == "symlink"
    assert by_path["screen-link"]["target"] == "screen.html"


def test_review_manifest_rejects_a_symlinked_protected_root(tmp_path):
    runner = _runner()
    source = tmp_path / "source"
    source.mkdir()
    link = tmp_path / "source-link"
    link.symlink_to(source, target_is_directory=True)

    with pytest.raises(ValueError, match="symlink"):
        runner.tree_manifest(link)


@pytest.mark.parametrize(
    "event",
    [
        {},
        {"schema_version": 1, "channel": "filesystem"},
        {"schema_version": 1, "channel": "unknown", "effect": "read"},
        {"schema_version": 1, "channel": "filesystem", "effect": "write"},
        {"schema_version": 1, "channel": "shell", "effect": "unknown"},
        {"schema_version": 1, "channel": "browser", "effect": "post"},
        {"schema_version": 2, "channel": "filesystem", "effect": "read"},
        {"schema_version": 1, "channel": "filesystem", "effect": "read", "extra": True},
    ],
)
def test_review_fixture_fails_closed_for_malformed_or_unknown_events(tmp_path, event):
    runner = _runner()
    source = tmp_path / "source"
    source.mkdir()
    manifest = runner.tree_manifest(source)
    with pytest.raises(runner.ReviewBoundaryViolation):
        runner.assert_review_boundary(manifest, manifest, [event])


def test_review_fixture_rejects_an_empty_trace(tmp_path):
    runner = _runner()
    source = tmp_path / "source"
    source.mkdir()
    manifest = runner.tree_manifest(source)
    with pytest.raises(runner.ReviewBoundaryViolation, match="trace_empty"):
        runner.assert_review_boundary(manifest, manifest, [])


@pytest.mark.parametrize("mutation", ["bytes", "mode", "symlink"])
def test_review_fixture_detects_source_tree_mutation(tmp_path, mutation):
    runner = _runner()
    source = tmp_path / "source"
    source.mkdir()
    file = source / "screen.html"
    file.write_text("before")
    target = source / "target"
    target.write_text("one")
    link = source / "link"
    link.symlink_to("target")
    before = runner.tree_manifest(source)

    if mutation == "bytes":
        file.write_text("after")
    elif mutation == "mode":
        os.chmod(file, 0o600)
    else:
        link.unlink()
        link.symlink_to("screen.html")

    after = runner.tree_manifest(source)
    with pytest.raises(runner.ReviewBoundaryViolation, match="tree_changed"):
        runner.assert_review_boundary(before, after, [_read_event()])


def test_review_report_must_resolve_outside_the_protected_root(tmp_path):
    runner = _runner()
    source = tmp_path / "source"
    source.mkdir()
    manifest = runner.tree_manifest(source)
    inside = source / "review-report.json"

    with pytest.raises(runner.ReviewBoundaryViolation, match="report_inside_protected_root"):
        runner.assert_review_boundary(
            manifest,
            manifest,
            [{"schema_version": 1, "channel": "output", "effect": "report-write", "path": str(inside)}],
            report_path=inside,
        )


def test_review_report_write_is_bound_to_one_outside_path(tmp_path):
    runner = _runner()
    source = tmp_path / "source"
    source.mkdir()
    report = tmp_path / "review-report.json"
    manifest = runner.tree_manifest(source)

    runner.assert_review_boundary(
        manifest,
        manifest,
        [{"schema_version": 1, "channel": "output", "effect": "report-write", "path": str(report)}],
        report_path=report,
    )

    with pytest.raises(runner.ReviewBoundaryViolation, match="report_path_mismatch"):
        runner.assert_review_boundary(
            manifest,
            manifest,
            [{"schema_version": 1, "channel": "output", "effect": "report-write", "path": str(tmp_path / "other.json")}],
            report_path=report,
        )


def test_review_boundary_cli_manifest_and_verify_contract(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    (source / "component.tsx").write_text("export const Component = () => null;\n")
    manifest_result = _run_boundary("manifest", "--root", str(source))
    assert manifest_result.returncode == 0, manifest_result.stderr
    manifest = json.loads(manifest_result.stdout)

    before_path = tmp_path / "before.json"
    after_path = tmp_path / "after.json"
    trace_path = tmp_path / "trace.json"
    _write_json(before_path, manifest)
    _write_json(after_path, manifest)
    _write_json(trace_path, [_read_event()])

    verified = _run_boundary(
        "verify", "--before", str(before_path), "--after", str(after_path), "--trace", str(trace_path)
    )
    assert verified.returncode == 0, verified.stderr
    assert json.loads(verified.stdout) == {"schema_version": 1, "status": "pass"}
    assert verified.stderr == ""


@pytest.mark.parametrize(
    "trace",
    [
        [],
        {"schema_version": 1},
        [{"schema_version": 1, "channel": "browser", "effect": "submit"}],
    ],
)
def test_review_boundary_cli_rejects_empty_malformed_and_forbidden_traces(tmp_path, trace):
    source = tmp_path / "source"
    source.mkdir()
    runner = _runner()
    manifest = runner.tree_manifest(source)
    before_path = tmp_path / "before.json"
    after_path = tmp_path / "after.json"
    trace_path = tmp_path / "trace.json"
    _write_json(before_path, manifest)
    _write_json(after_path, manifest)
    _write_json(trace_path, trace)

    result = _run_boundary(
        "verify", "--before", str(before_path), "--after", str(after_path), "--trace", str(trace_path)
    )

    assert result.returncode == 1
    assert json.loads(result.stdout)["status"] == "fail"
    assert result.stderr == ""


def test_review_boundary_cli_has_no_protected_tree_exclusion_option(tmp_path):
    source = tmp_path / "source"
    source.mkdir()
    result = _run_boundary(
        "manifest", "--root", str(source), "--exclude", "component.tsx"
    )
    assert result.returncode != 0
    assert "--exclude" in result.stderr
