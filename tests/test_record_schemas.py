"""Portable 2020-12 contracts for persisted Provenant records."""

from __future__ import annotations

from functools import lru_cache
import json
import os
from pathlib import Path
import re
import subprocess
import sys

import jsonschema
import pytest

from tests.support.dispatch import disable_memory_admission_floor
from tests.support.fake_cli import fake_bin, fake_codex
from tests.support.git import git_repo
from tests.support.modules import load_script
from tests.support.quota import snapshot as quota_snapshot, window as quota_window
from tests.support.runs import make_run

ROOT = Path(__file__).resolve().parents[1]
SCHEMAS = ROOT / "schemas"
SCHEMA_FILES = {path.name.removesuffix(".schema.json"): path
                for path in sorted(SCHEMAS.glob("*.schema.json"))}

# These identifiers describe presentation/report surfaces, not lane record contracts.
# Include legacy/read-only identifiers too: the scan deliberately catches all literals.
NON_RECORD_SCHEMAS = {
    "fabric.exec-plan.v1": "transient --plan-only CLI response consumed over stdout by the owner",
    "fabric.status.v1": "CLI status projection in run-status/owner-output and batch_run",
    "fabric.status.v2": "multi-host CLI status projection in fabric_lanes",
    "fabric.batch.v1": "legacy CLI batch projection recognised by owner-output",
    "fabric.runs.v1": "CLI run discovery projection in run-reader",
    "fabric.runs.v2": "multi-host CLI lane listing in fabric_lanes",
    "fabric.event.v1": "CLI event projection assembled from ledger rows in run-reader",
    "fabric.events.v1": "CLI event page and cursor in run-reader",
    "fabric.graph.v1": "CLI single-graph projection in graph/surface",
    "fabric.graphs.v1": "CLI graph listing in graph/surface",
    "fabric.inbox_digest.v1": "inbox summary response from store.inboxDigest",
    "fabric.catalogue.v1": "routing catalogue query response from model_route",
    "fabric.routes.v1": "route-pool listing response from model_route_pools",
    "fabric.cooldowns.active.v1": "active cooldown CLI report from model_route",
    "fabric.cooldown-reset.v1": "cooldown reset command result from model_route",
    "fabric.hosts.v1": "host listing response from fabric_hosts",
    "fabric.hosts.doctor.v1": "host diagnostic report from fabric_hosts",
    "provenant.quota.report.v1": "rendered quota report/cache, separate from account snapshots",
}
FUTURE_SCHEMA_IDS: set[str] = set()
RECORD_PATTERNS = {
    "canonical-attempt": "tasks/*/attempt-*/attempt.json",
    "dispatch-attempt": "dispatch/tasks/*/attempt-*/attempt.json",
    "adapter-receipt": "dispatch/tasks/*/attempt-*/adapter-receipt.json",
    "batch-summary": "dispatch/batches/batch-*/summary.json",
    "dispatch-owner": "dispatch-owner.json",
    "dispatch-status": "dispatch-status.json",
    "run-receipt": "RUN_RECEIPT.json",
    "watchdog-observation": "dispatch/tasks/*/attempt-*/watchdog.json",
    "wip-checkpoint": "dispatch/tasks/*/attempt-*/wip-checkpoint.json",
}
QUESTION = {"schema_version": 1, "record_type": "provenant-worker-terminal",
            "classification": "question",
            "question": {"code": "needs_input", "prompt": "Which source should I use?"}}


@lru_cache
def validator(name: str) -> jsonschema.Draft202012Validator:
    document = json.loads(SCHEMA_FILES[name].read_text(encoding="utf-8"))
    return jsonschema.Draft202012Validator(document, format_checker=jsonschema.FormatChecker())


def validate(name: str, value: object) -> None:
    validator(name).validate(value)


def reject(name: str, value: object) -> None:
    with pytest.raises(jsonschema.ValidationError):
        validate(name, value)


@pytest.fixture
def flow_root(tmp_path, monkeypatch):
    """Every flow and every subprocess sees only this flow's home and stores."""
    monkeypatch.chdir(tmp_path)
    for key, leaf in {
        "HOME": "home", "AGENT_FABRIC_INSTANCE_ROOT": ".agents",
        "AGENT_FABRIC_STATE_ROOT": "state", "AGENT_FABRIC_STATE_DIRECTORY": "state",
        "CODEX_HOME": "codex-home", "CLAUDE_CONFIG_DIR": "claude-home",
        "XDG_STATE_HOME": "xdg-state", "PROVENANT_PROFILE_ROOT": "profiles",
    }.items():
        path = tmp_path / leaf
        path.mkdir(exist_ok=True)
        monkeypatch.setenv(key, str(path))
    monkeypatch.setenv("FABRIC_COOLDOWNS_PATH", str(tmp_path / "cooldowns.json"))
    monkeypatch.setenv("AGENT_FABRIC_ROUTE_HEALTH_PATH", str(tmp_path / "route-health.json"))
    monkeypatch.setenv("PROVENANT_NO_OS_CONFINEMENT", "1")
    disable_memory_admission_floor(tmp_path)
    git_repo(tmp_path, files={".gitignore": "*\n!.gitignore\n"})
    return tmp_path


def run_command(args, root):
    # Pass all four isolation roots explicitly, including to run initialisation
    # (make_run inherits this same pinned environment).
    env = os.environ.copy()
    for key in ("HOME", "AGENT_FABRIC_INSTANCE_ROOT", "AGENT_FABRIC_STATE_ROOT", "CODEX_HOME"):
        assert Path(env[key]).is_relative_to(root)
    env["PATH"] = f"{root / 'bin'}:{ROOT / 'scripts'}:{env['PATH']}"
    result = subprocess.run(args, cwd=root, env=env, capture_output=True, text=True, timeout=60)
    return result


def provider(root, body):
    bindir = fake_bin(root)
    fake_codex(bindir, body=body)


def write_dispatch(root, flow="success"):
    bodies = {
        "success": "cat >/dev/null\nprintf 'OK\\n'\n",
        "failed": "cat >/dev/null\nprintf 'provider crashed\\n' >&2\nexit 7\n",
        "timed-out": "cat >/dev/null\nexec sleep 10\n",
        "quota-limited": "cat >/dev/null\nprintf 'usage limit reached\\n' >&2\nexit 1\n",
        "question": "cat >/dev/null\nprintf '%s\\n' '" + json.dumps(QUESTION) + "'\n",
    }
    provider(root, bodies[flow])
    run = make_run(root, f"schema-{flow}")
    prompt = root / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    args = [sys.executable, str(ROOT / "skills/orchestrate/scripts/dispatch_run.py"),
            "--run-dir", str(run), "--task-id", "task-1", "--adapter", "codex",
            "--prompt-file", str(prompt), "--orchestrator-family", "openai", "--alias", "senior",
            "--role", "worker", "--fallback", "false"]
    if flow == "timed-out":
        args += ["--timeout-seconds", "1"]
    result = run_command(args, root)
    assert result.returncode == (0 if flow == "success" else 1), result.stderr[-4000:]
    return run


def validate_run_records(run):
    seen = set()
    for name, pattern in RECORD_PATTERNS.items():
        for path in sorted(run.glob(pattern)):
            value = json.loads(path.read_text(encoding="utf-8"))
            validate(name, value)
            reject(name, {})
            if name == "run-receipt":
                reject(name, {**value, "pair": {"mode": "invented"}})
                reject(name, {**value, "closed_panes": [{}]})
            seen.add(name)
    for path in sorted(run.glob("dispatch/tasks/*/attempt-*/attempt.sha256")):
        digest, filename = path.read_text().strip().split(maxsplit=1)
        validate("attempt-digest-sidecar", {"digest": digest, "filename": filename})
        reject("attempt-digest-sidecar", {"digest": "sha256:bad", "filename": filename})
        seen.add("attempt-digest-sidecar")
    for line in (run / "MANIFEST.md").read_text().splitlines():
        if line.startswith("| dispatch-"):
            cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
            row = dict(zip(("id", "path", "topic", "produced_by", "date", "status", "retention", "supersedes"),
                           cells, strict=True))
            validate("manifest-row", row)
            reject("manifest-row", {**row, "status": "invented"})
            seen.add("manifest-row")
    return seen


@pytest.mark.parametrize("flow,status,outcome", [
    ("success", "ok", "ok"), ("failed", "failed", "failed"),
    ("timed-out", "timed_out", "timeout"), ("quota-limited", "failed", "usage_limited"),
    ("question", "blocked", "question"),
])
def test_dispatch_records(flow_root, flow, status, outcome):
    run = write_dispatch(flow_root, flow)
    seen = validate_run_records(run)
    assert {"canonical-attempt", "dispatch-attempt", "adapter-receipt", "run-receipt",
            "attempt-digest-sidecar", "manifest-row"} <= seen
    value = json.loads((run / "dispatch/tasks/task-1/attempt-001/attempt.json").read_text())
    assert value["status"] == status
    assert value["outcome"] == outcome
    reject("dispatch-attempt", {**value, "unexpected_field": True})
    reject("dispatch-attempt", {**value, "started_at": "yesterday"})
    reject("dispatch-attempt", {**value, "prompt": {"path": "", "digest": "bad"}})
    canonical = json.loads((run / "tasks/task-1/attempt-001/attempt.json").read_text())
    reject("canonical-attempt", {**canonical, "unexpected_field": True})
    reject("canonical-attempt", {**canonical, "status": "invented"})
    reject("canonical-attempt", {**canonical, "paths": {}})
    nullable_account = {**canonical, "account": None}
    validate("canonical-attempt", nullable_account)
    if flow == "question":
        envelope = json.loads((run / "dispatch/tasks/task-1/attempt-001/result.md").read_text())
        validate("worker-terminal-envelope", envelope)
        reject("worker-terminal-envelope", {**envelope, "question": {"code": "needs_input", "prompt": ""}})
    if flow in {"failed", "quota-limited"}:
        assert "wip-checkpoint" in seen


def test_active_attempt_records(flow_root):
    dispatch = load_script(ROOT / "skills/orchestrate/scripts/dispatch_run.py", "schema_active_dispatch")
    run = make_run(flow_root, "schema-active")
    prompt = flow_root / "prompt.md"
    prompt.write_text("test\n")
    args = dispatch.parser().parse_args([
        "--run-dir", str(run), "--task-id", "active", "--adapter", "codex",
        "--prompt-file", str(prompt), "--alias", "senior", "--role", "worker",
    ])
    attempt = run / "dispatch/tasks/active/attempt-001"
    row = dispatch.contract_row(args, run, 1, attempt, {}, "2026-10-23T00:00:00Z")
    dispatch.publish_contract(run, row)
    value = json.loads((run / "tasks/active/attempt-001/attempt.json").read_text())
    assert value["state"] == "running"
    assert "queued_seconds" not in value["timing"]
    validate("canonical-attempt", value)
    reject("canonical-attempt", {**value, "timing": {"phases": [], "queued_seconds": 0}})


def test_batch_records(flow_root):
    provider(flow_root, "cat >/dev/null\nprintf 'OK\\n'\n")
    run = make_run(flow_root, "schema-batch")
    prompt = flow_root / "prompt.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    manifest = flow_root / "tasks.json"
    manifest.write_text(json.dumps({"schema_version": 1, "tasks": [
        {"id": "task-1", "adapter": "codex", "alias": "senior", "prompt_file": str(prompt)}
    ]}), encoding="utf-8")
    result = run_command([sys.executable, str(ROOT / "skills/orchestrate/scripts/batch_run.py"),
                          "--run-dir", str(run), "--manifest", str(manifest)], flow_root)
    assert result.returncode == 0, result.stderr[-4000:]
    assert "batch-summary" in validate_run_records(run)
    summary = json.loads((run / "dispatch/batches/batch-001/summary.json").read_text())
    reject("batch-summary", {**summary, "counts": {"ok": "one"}})
    reject("batch-summary", {**summary, "tasks": [{}]})


def test_adapter_error_receipt(flow_root):
    # The adapter's exception writer has a separate, deliberately small shape.
    provider(flow_root, "exit 7\n")
    prompt = flow_root / "prompt.md"
    prompt.write_text("test\n")
    result = run_command([sys.executable, str(ROOT / "skills/orchestrate/scripts/provider_exec.py"),
                          "--adapter", "codex", "--route-file", str(flow_root / "missing-route.json"),
                          "--prompt-file", str(prompt), "--out", str(flow_root / "result.md")], flow_root)
    assert result.returncode == 2, result.stderr[-4000:]
    value = json.loads(result.stdout)
    assert value["status"] == "output_write_error"
    assert value["fix"]
    validate("adapter-receipt", value)
    reject("adapter-receipt", {})
    reject("adapter-receipt", {**value, "fix": 17})


def test_watchdog_records(flow_root):
    watchdog = load_script(ROOT / "skills/orchestrate/scripts/stall_watchdog.py", "schema_watchdog")
    attempt = flow_root / "attempt"
    attempt.mkdir()
    (attempt / "events.jsonl").write_text('{"type":"tool_call","tool":"read"}\n')
    watchdog.write_observation(attempt, flow_root, os.getpid(), True, 5)
    with subprocess.Popen([sys.executable, "-c", "pass"], env=os.environ.copy()) as child:
        assert child.wait(timeout=5) == 0
    watchdog.checkpoint(attempt, flow_root, child.pid, include_git=False)
    for name, filename in (("watchdog-observation", "watchdog.json"), ("wip-checkpoint", "wip-checkpoint.json")):
        row = json.loads((attempt / filename).read_text())
        validate(name, row)
        reject(name, {**row, "process_alive": "yes"})


def test_quota_records(flow_root, monkeypatch):
    quota = load_script(ROOT / "scripts/provider_quota.py", "quota_record_schema_test")
    monkeypatch.setattr(quota, "telemetry_host", lambda: "schema-host")
    value = quota_snapshot(quota, host="schema-host", windows=(quota_window(75, 60),))
    quota.save_snapshot(value)
    path = quota.cache_path(value["provider"], value["account"], value["host"])
    validate("quota-snapshot", json.loads(path.read_text()))
    reject("quota-snapshot", {**value, "windows": [{"used_percent": "seventy-five"}]})
    # Exercise populated provider fields through the refresh writer, not only
    # the minimal snapshot builder. Stub the provider boundary only.
    monkeypatch.setenv("PROVENANT_QUOTA", "1")
    raw = {
        "source": "test", "plan": "pro", "windows": [quota_window(45, 60)],
        "usage_credits": [{"kind": "credits", "balance": 50, "currency": "USD",
                           "unit": "credits", "expires_at": "2027-01-01T00:00:00Z"}],  # timebomb: allow schema fixture dates, never compared with now
        "resets": {"available": 2, "expires_at": ["2027-01-01T00:00:00Z"], "source": "provider"},  # timebomb: allow schema fixture dates, never compared with now
        "allowance": {"ordinary_usage_allowed": True, "spend_control_reached": False},
        "credits": {"reset_credits": 2, "source": "provider"},
    }
    monkeypatch.setattr(quota.provider("codex"), "fetch", lambda account, io, now: raw)
    account = quota.Account("codex", "populated", flow_root / "codex-home", True)
    populated = quota.refresh(account, quota.parse_time(value["observed_at"]), force=True)
    assert populated["status"] == "ok", populated.get("error")
    assert populated["usage_credits"] and populated["resets"] and populated["credit_anchor"]
    validate("quota-snapshot", populated)
    persisted = quota.cache_path(account.provider, account.label, "schema-host", account.identity_dir)
    validate("quota-snapshot", json.loads(persisted.read_text()))
    reject("quota-snapshot", {**populated, "usage_credits": {}})
    reject("quota-snapshot", {**populated, "resets": []})
    reject("quota-snapshot", {**populated, "allowance": {"ordinary_usage_allowed": "yes"}})


@pytest.mark.parametrize("name", ["model-routing", "model-preferences", "delivery-profiles"])
def test_config_schemas(name):
    value = json.loads((ROOT / "config" / f"{name}.json").read_text())
    validate(name, value)
    field = {"model-routing": "routes", "model-preferences": "roles", "delivery-profiles": "profiles"}[name]
    reject(name, {**value, field: []})


@pytest.mark.parametrize("name", sorted(path.name.removesuffix(".valid.json")
                                        for path in (SCHEMAS / "fixtures").glob("*.valid.json")))
def test_spec_and_writer_fixtures(name):
    for suffix, check in (("valid", validate), ("invalid", reject)):
        check(name, json.loads((SCHEMAS / "fixtures" / f"{name}.{suffix}.json").read_text()))


def test_schema_inventory():
    documents = [validator(name).schema for name in SCHEMA_FILES]
    for document in documents:
        jsonschema.Draft202012Validator.check_schema(document)
    # Broad scan includes object literals, keyword updates, constants,
    # assignments and TS unions, rather than assuming every writer uses a
    # quoted `schema:` key. Unrelated versioned IDs need a reasoned allowlist
    # entry after checking whether they are persisted record writers.
    pattern = re.compile(r"['\"]([a-z][a-z0-9_.-]*\.v\d+)['\"]")
    observed = set()
    for directory in ("scripts", "skills", "runtime/fabric/src"):
        for path in (ROOT / directory).rglob("*"):
            relative_parts = path.relative_to(ROOT).parts
            if (path.is_file() and not any(part.startswith(".") or part == "node_modules" for part in relative_parts)
                    and path.suffix in {"", ".py", ".ts", ".tsx", ".sh", ".js", ".mjs", ".cjs"}):
                observed.update(pattern.findall(path.read_text(encoding="utf-8", errors="ignore")))
    declared = {document.get("properties", {}).get("schema", {}).get("const") for document in documents}
    declared.discard(None)
    assert "date-time" in jsonschema.FormatChecker().checkers
    fixture_names = {path.name.removesuffix(".valid.json")
                     for path in (SCHEMAS / "fixtures").glob("*.valid.json")}
    covered = set(RECORD_PATTERNS) | fixture_names | {
        "model-routing", "model-preferences", "delivery-profiles", "quota-snapshot",
        "worker-terminal-envelope", "attempt-digest-sidecar", "manifest-row",
    }
    assert set(SCHEMA_FILES) == covered, "each schema needs a flow or paired fixtures"
    assert NON_RECORD_SCHEMAS.keys() <= observed, "remove stale report exceptions"
    assert all(NON_RECORD_SCHEMAS.values())
    assert not (NON_RECORD_SCHEMAS.keys() & declared)
    assert observed - NON_RECORD_SCHEMAS.keys() == declared - FUTURE_SCHEMA_IDS, (
        "unclassified versioned identifiers: add persisted record schemas, or after checking "
        "record-writing contexts add non-record/report identifiers to NON_RECORD_SCHEMAS with a reason"
    )
    for name in ("launch-spec", "observer-record", "supervisor-exit", "supervisor-state"):
        assert (SCHEMAS / "fixtures" / f"{name}.valid.json").is_file()
        assert (SCHEMAS / "fixtures" / f"{name}.invalid.json").is_file()
