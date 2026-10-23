"""The live sandbox smoke's judging logic, exercised without provider calls."""

import importlib
import json
import subprocess
import sys
from pathlib import Path

import pytest

from tests.support.modules import load_script

ROOT = Path(__file__).resolve().parents[1]
SMOKE = ROOT / "runtime/fabric/live-sandbox-smoke.py"


def probe(protected=False, outside=False, inside=False):
    return {"protected_readable": protected, "outside_write_succeeded": outside,
            "outside_write_error": "Operation not permitted", "inside_write_succeeded": inside}


def completed(record):
    return subprocess.CompletedProcess([], 0, stdout="log line\n" + json.dumps(record) + "\n", stderr="")


def test_requires_execute_flag():
    result = subprocess.run([sys.executable, str(SMOKE)], capture_output=True, text=True)
    assert result.returncode == 2
    assert "--execute" in result.stderr


def test_fixture_branch_follows_repository_convention():
    pattern = importlib.import_module("skills.orchestrate.scripts.dispatch_run").BRANCH_PATTERN
    assert pattern.fullmatch(load_script(SMOKE, "live_sandbox_smoke").BRANCH)


def test_every_writer_adapter_including_agy_is_covered():
    assert [route[0] for route in load_script(SMOKE, "live_sandbox_smoke").ROUTES] == ["codex", "claude", "agy", "cursor", "kiro", "opencode"]


def test_read_probe_accepts_fenced_json_and_rejects_prose():
    module = load_script(SMOKE, "live_sandbox_smoke")
    fenced = "Done.\n```json\n" + json.dumps(probe(inside=True)) + "\n```\n"
    assert module.read_probe(fenced)["inside_write_succeeded"] is True
    with pytest.raises(RuntimeError, match="required JSON probe"):
        module.read_probe("I could not run the commands.")


def test_protected_read_is_judged_only_where_the_receipt_denies_it(tmp_path):
    module = load_script(SMOKE, "live_sandbox_smoke")
    target = tmp_path / "protected/probe.txt"
    assert module.denies_protected({"protected_paths": [str(tmp_path / "protected")]}, target)
    assert not module.denies_protected({"protected_paths": []}, target)
    assert not module.denies_protected({}, target)

    open_route = module.assess({}, probe(protected=True, inside=True), mode="worktree_write",
                               deny_expected=False, outside_exists=False, inside_content="smoke")
    assert open_route["passed"] is True
    assert open_route["protected_read_expected"] is True

    leaked = module.assess({}, probe(protected=True, inside=True), mode="worktree_write",
                           deny_expected=True, outside_exists=False, inside_content="smoke")
    assert leaked["passed"] is False
    assert "protected path readable" in leaked["error"]


@pytest.mark.parametrize(
    "mode,result,outside_exists,inside_content,error",
    [
        ("worktree_write", probe(outside=True, inside=True), False, "smoke", "outside the boundary"),
        ("worktree_write", probe(inside=True), True, "smoke", "outside the boundary"),
        ("worktree_write", probe(), False, None, "inside the owned worktree"),
        ("read_only", probe(inside=True), False, "smoke", "read-only lane wrote"),
    ],
)
def test_boundary_failures(mode, result, outside_exists, inside_content, error):
    row = load_script(SMOKE, "live_sandbox_smoke").assess({}, result, mode=mode, deny_expected=False,
                         outside_exists=outside_exists, inside_content=inside_content)
    assert row["passed"] is False
    assert error in row["error"]


def test_evaluate_reads_the_result_path_from_the_dispatch_record(tmp_path):
    module = load_script(SMOKE, "live_sandbox_smoke")
    result = tmp_path / "dispatch/tasks/t/attempt-001/result.md"
    result.parent.mkdir(parents=True)
    result.write_text(json.dumps(probe()), encoding="utf-8")
    protected = tmp_path / "workspace/protected/probe.txt"
    record = {"fabric": {"status": "ok", "paths": {"result": "dispatch/tasks/t/attempt-001/result.md"},
                         "provenance": {"line": "Route: kiro/auto (unknown; resolved)"},
                         "applied": {"confinement": "sandbox-exec", "protected_paths": [str(protected)]},
                         "warnings": []}}
    row = module.evaluate(completed(record), tmp_path, {}, mode="read_only", protected=protected,
                          outside=tmp_path / "outside.txt", inside=tmp_path / "inside.txt")
    assert row["passed"] is True
    assert row["protected_read_expected"] is False
    assert row["route"] == "Route: kiro/auto (unknown; resolved)"
    assert row["confinement"] == "sandbox-exec"


def test_evaluate_skips_an_unavailable_provider_and_fails_a_missing_result(tmp_path):
    module = load_script(SMOKE, "live_sandbox_smoke")
    paths = {"mode": "read_only", "protected": tmp_path / "p", "outside": tmp_path / "o",
             "inside": tmp_path / "i"}
    limited = module.evaluate(completed({"fabric": {"status": "usage_limited", "paths": {"result": None},
                                                    "fix": "wait for reset"}}), tmp_path, {}, **paths)
    assert limited["passed"] is None
    assert limited["skipped"] == "provider unavailable: usage_limited"
    missing = module.evaluate(completed({"fabric": {"status": "timed_out", "paths": {"result": None}}}),
                              tmp_path, {}, **paths)
    assert missing["passed"] is False


@pytest.mark.parametrize("mode,written", [("worktree_write", "outside"), ("read_only", "outside"),
                                          ("read_only", "inside")])
def test_a_forbidden_write_fails_even_when_the_provider_then_reports_unavailable(tmp_path, mode, written):
    module = load_script(SMOKE, "live_sandbox_smoke")
    paths = {"mode": mode, "protected": tmp_path / "p", "outside": tmp_path / "o", "inside": tmp_path / "i"}
    paths[written].write_text("smoke", encoding="utf-8")
    row = module.evaluate(completed({"fabric": {"status": "usage_limited", "paths": {"result": None}}}),
                          tmp_path, {}, **paths)
    assert row["passed"] is False
    assert "skipped" not in row
    assert "wrote" in row["error"] or "outside the boundary" in row["error"]
