"""Portable 2020-12 contracts for records written by Provenant."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import sqlite3
import subprocess
import sys
from datetime import timedelta

import jsonschema
from tests.support.dispatch import disable_memory_admission_floor
from tests.support.fake_cli import fake_bin, fake_codex
from tests.support.git import git_repo
from tests.support.quota import snapshot as quota_snapshot, window as quota_window
from tests.support.modules import load_script
from tests.support.runs import make_run

ROOT = Path(__file__).resolve().parents[1]
SCHEMAS = ROOT / "schemas"
SCHEMA_FILES = {
    "canonical-attempt": "canonical-attempt.schema.json",
    "dispatch-attempt": "dispatch-attempt.schema.json",
    "adapter-receipt": "adapter-receipt.schema.json",
    "worker-terminal-envelope": "worker-terminal-envelope.schema.json",
    "attempt-digest-sidecar": "attempt-digest-sidecar.schema.json",
    "batch-summary": "batch-summary.schema.json",
    "manifest-row": "manifest-row.schema.json",
    "dispatch-status": "dispatch-status.schema.json",
    "dispatch-owner": "dispatch-owner.schema.json",
    "run-receipt": "run-receipt.schema.json",
    "supervisor-exit": "supervisor-exit.schema.json",
    "supervisor-state": "supervisor-state.schema.json",
    "observer-record": "observer-record.schema.json",
    "launch-spec": "launch-spec.schema.json",
    "watchdog-observation": "watchdog-observation.schema.json",
    "wip-checkpoint": "wip-checkpoint.schema.json",
    "quota-snapshot": "quota-snapshot.schema.json",
    "telemetry-usage-row": "telemetry-usage-row.schema.json",
    "telemetry-event-row": "telemetry-event-row.schema.json",
    "telemetry-quota-series-row": "telemetry-quota-series-row.schema.json",
    "telemetry-quota-event-row": "telemetry-quota-event-row.schema.json",
    "model-routing": "model-routing.schema.json",
    "model-preferences": "model-preferences.schema.json",
    "delivery-profiles": "delivery-profiles.schema.json",
}


def schema(name: str) -> dict:
    return json.loads((SCHEMAS / SCHEMA_FILES[name]).read_text(encoding="utf-8"))


def validate(name: str, value: object) -> None:
    jsonschema.validate(value, schema(name))


def _write_dispatch(tmp_path: Path, name: str) -> Path:
    disable_memory_admission_floor(tmp_path)
    run_dir = make_run(tmp_path, name)
    prompt = tmp_path / f"{name}.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    provider_root = tmp_path / f"{name}-provider"
    provider_root.mkdir()
    bin_dir = fake_bin(provider_root)
    fake_codex(bin_dir, body="cat >/dev/null\nprintf 'OK\\n'\n")
    env = {**os.environ, "PATH": f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}",
           "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT)}
    result = subprocess.run(
        [sys.executable, str(ROOT / "skills/orchestrate/scripts/dispatch_run.py"),
         "--run-dir", str(run_dir), "--task-id", "task-1", "--adapter", "codex",
         "--prompt-file", str(prompt), "--orchestrator-family", "openai", "--alias", "senior",
         "--role", "worker"],
        cwd=tmp_path, env=env, capture_output=True, text=True,
    )
    stderr_path = run_dir / "dispatch/tasks/task-1/attempt-001/stderr.log"
    detail = stderr_path.read_text(encoding="utf-8", errors="replace") if stderr_path.exists() else ""
    assert result.returncode == 0, result.stderr + detail + result.stdout
    return run_dir


def _validate_run_records(run_dir: Path) -> None:
    validate("run-receipt", json.loads((run_dir / "RUN_RECEIPT.json").read_text()))
    status = run_dir / "dispatch-status.json"
    owner = run_dir / "dispatch-owner.json"
    if status.exists():
        validate("dispatch-status", json.loads(status.read_text()))
    if owner.exists():
        validate("dispatch-owner", json.loads(owner.read_text()))
    for path in run_dir.rglob("*.json"):
        value = json.loads(path.read_text(encoding="utf-8"))
        rel = path.relative_to(run_dir).as_posix()
        if path.name == "attempt.json":
            validate("canonical-attempt" if rel.startswith("tasks/") else "dispatch-attempt", value)
        elif path.name == "adapter-receipt.json":
            validate("adapter-receipt", value)
        elif path.name == "summary.json":
            validate("batch-summary", value)
        elif path.name == "dispatch-owner.json":
            validate("dispatch-owner", value)
        elif path.name == "dispatch-status.json":
            validate("dispatch-status", value)
        elif path.name == "RUN_RECEIPT.json":
            validate("run-receipt", value)
    for path in run_dir.rglob("attempt.sha256"):
        digest, filename = path.read_text().strip().split(maxsplit=1)
        validate("attempt-digest-sidecar", {"digest": digest, "filename": filename})
    manifest = run_dir / "MANIFEST.md"
    for line in manifest.read_text().splitlines():
        if not line.startswith("| dispatch-"):
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        validate("manifest-row", dict(zip(
            ("id", "path", "topic", "produced_by", "date", "status", "retention", "supersedes"), cells,
            strict=True,
        )))


def test_config_schemas_and_schema_inventory():
    for name, path in (("model-routing", "model-routing.json"),
                       ("model-preferences", "model-preferences.json"),
                       ("delivery-profiles", "delivery-profiles.json")):
        validate(name, json.loads((ROOT / "config" / path).read_text(encoding="utf-8")))

    documents = [json.loads(path.read_text(encoding="utf-8")) for path in sorted(SCHEMAS.glob("*.schema.json"))]
    assert documents
    assert {path.name for path in SCHEMAS.glob("*.schema.json")} == set(SCHEMA_FILES.values())
    for document in documents:
        jsonschema.Draft202012Validator.check_schema(document)

    # Inventory the schema identifiers emitted by the selected on-disk writers.
    writers = (
        ROOT / "skills/orchestrate/scripts/dispatch_run.py",
        ROOT / "skills/orchestrate/scripts/stall_watchdog.py",
        ROOT / "scripts/provider_quota.py",
    )
    emitted = set()
    pattern = re.compile(r"['\"]schema['\"]\s*:\s*['\"]([^'\"]+\.v\d+)['\"]")
    for path in writers:
        emitted.update(pattern.findall(path.read_text(encoding="utf-8")))
    quota_source = (ROOT / "scripts/provider_quota.py").read_text(encoding="utf-8")
    emitted.update(re.findall(r"(?m)^SCHEMA\s*=\s*['\"]([^'\"]+\.v\d+)['\"]", quota_source))
    schema_ids = {doc.get("properties", {}).get("schema", {}).get("const") for doc in documents}
    assert emitted <= schema_ids


def test_real_dispatch_batch_quota_and_telemetry_records_validate(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    git_repo(tmp_path, files={".gitignore": "*\n!.gitignore\n"})
    run_dir = _write_dispatch(tmp_path, "schema-dispatch")
    _validate_run_records(run_dir)

    # A one-task batch exercises the same record writers through batch custody.
    batch_run = make_run(tmp_path, "schema-batch")
    prompt = tmp_path / "batch.md"
    prompt.write_text("Reply exactly OK\n", encoding="utf-8")
    manifest = tmp_path / "tasks.json"
    manifest.write_text(json.dumps({"schema_version": 1, "tasks": [
        {"id": "task-1", "adapter": "codex", "alias": "senior", "prompt_file": str(prompt)}
    ]}), encoding="utf-8")
    provider_root = tmp_path / "batch-provider"
    provider_root.mkdir()
    bin_dir = fake_bin(provider_root)
    fake_codex(bin_dir, body="cat >/dev/null\nprintf 'OK\\n'\n")
    env = os.environ.copy()
    env.update({"PATH": f"{bin_dir}:{ROOT / 'scripts'}:{env['PATH']}",
                "AGENT_FABRIC_PRODUCT_ROOT": str(ROOT)})
    result = subprocess.run(
        [sys.executable, str(ROOT / "skills/orchestrate/scripts/batch_run.py"),
         "--run-dir", str(batch_run), "--manifest", str(manifest)],
        cwd=tmp_path, env=env, capture_output=True, text=True,
    )
    assert result.returncode == 0, result.stderr + result.stdout
    _validate_run_records(batch_run)

    quota = load_script(ROOT / "scripts/provider_quota.py", "quota_record_schema_test")
    quota.state_root = lambda: tmp_path / "state"
    quota.telemetry_host = lambda: "schema-host"
    value = quota_snapshot(quota, windows=(quota_window(75, 60),))
    quota.save_snapshot(value)
    snapshot_path = next((tmp_path / "state").rglob("quota/*.json"), None)
    assert snapshot_path is not None
    validate("quota-snapshot", json.loads(snapshot_path.read_text()))

    state = tmp_path / "telemetry-state"
    state.mkdir()
    monkeypatch.setenv("AGENT_FABRIC_STATE_ROOT", str(state))
    quota.record_series(value, {"plan": "pro", "plan_tier": "pro", "credit_balance": 0})
    previous = {**value, "observed_at": "2026-10-22T00:00:00Z",
                "windows": [quota_window(95, 10080, reset_in=timedelta(days=1))]}
    current = {**value, "observed_at": "2026-10-23T00:00:00Z",
               "windows": [quota_window(10, 10080, reset_in=timedelta(days=2))]}
    quota.record_events(previous, current)
    source_attempt = run_dir / "tasks/task-1/attempt-001/attempt.json"
    retained_attempt = tmp_path / ".agent-run/runs/schema-dispatch/tasks/task-1/attempt-001/attempt.json"
    retained_attempt.parent.mkdir(parents=True)
    retained_attempt.write_bytes(source_attempt.read_bytes())
    ingest_env = {**env, "AGENT_FABRIC_STATE_ROOT": str(state),
                  "AGENT_FABRIC_STATE_DIRECTORY": str(state),
                  "AGENT_FABRIC_INSTANCE_ROOT": str(tmp_path / ".agents"),
                  "HOME": str(tmp_path / "home")}
    (tmp_path / "home").mkdir()
    ingest = subprocess.run([str(ROOT / "scripts/provenant"), "telemetry", "ingest"],
                            cwd=tmp_path, env=ingest_env, capture_output=True, text=True)
    assert ingest.returncode == 0, ingest.stderr
    with sqlite3.connect(state / "telemetry.sqlite3") as db:
        db.row_factory = sqlite3.Row
        row = db.execute("SELECT * FROM usage WHERE source='fabric' LIMIT 1").fetchone()
        assert row is not None
        validate("telemetry-usage-row", dict(row))
        row = db.execute("SELECT * FROM events WHERE kind='lane.attempt' LIMIT 1").fetchone()
        assert row is not None
        validate("telemetry-event-row", dict(row))
        row = db.execute("SELECT * FROM quota_series LIMIT 1").fetchone()
        assert row is not None
        validate("telemetry-quota-series-row", dict(row))
        row = db.execute("SELECT * FROM quota_events LIMIT 1").fetchone()
        assert row is not None
        validate("telemetry-quota-event-row", dict(row))
