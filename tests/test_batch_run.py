"""Executable contract tests for fixed bounded ordinary batches."""

from __future__ import annotations

import importlib.util
import json
import os
from pathlib import Path
import signal
import stat
import subprocess
import textwrap
import time

import pytest


ROOT = Path(__file__).resolve().parents[1]
BATCH = ROOT / "skills/orchestrate/scripts/batch_run.py"
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"


def write_executable(path: Path, body: str) -> None:
    path.write_text(textwrap.dedent(body).lstrip(), encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def make_run(tmp_path: Path, name: str) -> Path:
    return Path(subprocess.check_output([str(INIT), str(tmp_path / ".agent-run" / name)], text=True).strip())


def load_module():
    spec = importlib.util.spec_from_file_location("batch_run_under_test", BATCH)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fake_dispatch(path: Path) -> None:
    write_executable(path, """
        #!/usr/bin/env python3
        import argparse, fcntl, json, os, pathlib, signal, time
        p = argparse.ArgumentParser()
        p.add_argument('--task-id', required=True)
        p.add_argument('--prompt-file', required=True)
        p.add_argument('--run-dir', required=True)
        p.add_argument('--timeout')
        p.add_argument('--out')
        p.add_argument('--adapter')
        p.add_argument('--role')
        p.add_argument('--alias')
        p.add_argument('--intent')
        ns, _ = p.parse_known_args()
        prompt = pathlib.Path(ns.prompt_file).read_text()
        values = dict(line.split('=', 1) for line in prompt.splitlines() if '=' in line)
        counter = pathlib.Path(os.environ['BATCH_COUNTER'])
        active = counter.with_name('active')
        maximum = counter.with_name('maximum')
        with counter.with_name('lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            current = int(active.read_text()) if active.exists() else 0
            active.write_text(str(current + 1))
            maximum.write_text(str(max(int(maximum.read_text()) if maximum.exists() else 0, current + 1)))
        def leave(*_):
            with counter.with_name('lock').open('w') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX)
                active.write_text(str(max(0, int(active.read_text()) - 1)))
            raise SystemExit(143)
        signal.signal(signal.SIGTERM, leave)
        signal.signal(signal.SIGHUP, leave)
        time.sleep(float(values.get('sleep', '0.01')))
        with counter.with_name('lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            active.write_text(str(max(0, int(active.read_text()) - 1)))
        status = values.get('status', 'succeeded')
        result = None if status in ('empty', 'timed_out', 'failed') else {'path': 'dispatch/result.md', 'digest': 'sha256:test'}
        record = {'status': status, 'task_id': ns.task_id, 'attempt_id': 'attempt-001',
                  'attempt_path': 'dispatch/tasks/' + ns.task_id + '/attempt-001/attempt.json',
                  'result': result}
        print(json.dumps(record))
        raise SystemExit(0 if status == 'succeeded' else 1)
    """)


def task_manifest(tmp_path: Path, tasks: list[dict]) -> Path:
    manifest = tmp_path / "tasks.json"
    manifest.write_text(json.dumps({'schema_version': 1, 'tasks': tasks}) + "\n", encoding="utf-8")
    return manifest


def task(tmp_path: Path, task_id: str, **values: str) -> dict:
    prompt = tmp_path / f"{task_id}.md"
    prompt.write_text("\n".join(f"{key}={value}" for key, value in values.items()) + "\n", encoding="utf-8")
    return {'id': task_id, 'prompt_file': str(prompt), 'adapter': 'codex', 'alias': 'scout', 'role': 'worker'}


def args(module, run_dir: Path, manifest: Path, concurrency: int = 3):
    return module.parser().parse_args([
        '--run-dir', str(run_dir), '--manifest', str(manifest),
        '--concurrency', str(concurrency),
    ])


def test_fixed_batch_caps_eight_tasks_and_produces_reducer_inputs(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'eight')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))
    manifest = task_manifest(tmp_path, [task(tmp_path, f'task-{i}', sleep='0.03') for i in range(8)])

    assert module.batch(args(module, run_dir, manifest, 3)) == 0
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    assert summary['status'] == 'completed'
    assert len(summary['tasks']) == 8
    assert int((tmp_path / 'maximum').read_text()) <= 3
    assert {entry['task_id'] for entry in summary['reducer_inputs']} == {f'task-{i}' for i in range(8)}


def test_batch_continues_after_mixed_success_failure_empty_and_timeout(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'mixed')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))
    manifest = task_manifest(tmp_path, [
        task(tmp_path, 'ok', status='succeeded'), task(tmp_path, 'bad', status='failed'),
        task(tmp_path, 'empty', status='empty'), task(tmp_path, 'slow', status='timed_out'),
    ])

    assert module.batch(args(module, run_dir, manifest, 2)) == 1
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    assert {entry['task_id']: entry['status'] for entry in summary['tasks']} == {
        'ok': 'succeeded', 'bad': 'failed', 'empty': 'empty', 'slow': 'timed_out',
    }


@pytest.mark.parametrize('bad_tasks', [
    [{'id': 'same', 'prompt_file': 'a', 'adapter': 'codex', 'alias': 'scout', 'role': 'worker'},
     {'id': 'same', 'prompt_file': 'b', 'adapter': 'gemini', 'alias': 'scout', 'role': 'worker'}],
    [{'id': 'bad id', 'prompt_file': 'a', 'adapter': 'codex', 'alias': 'scout', 'role': 'worker'}],
])
def test_batch_rejects_duplicate_or_malformed_task_schema(tmp_path, bad_tasks):
    module = load_module()
    manifest = task_manifest(tmp_path, bad_tasks)
    with pytest.raises(module.BatchInputError):
        module.load_manifest(manifest)


def test_batch_rejects_source_writer_without_explicit_isolation(tmp_path):
    module = load_module()
    value = task(tmp_path, 'writer', source_writing=True)
    with pytest.raises(module.BatchInputError, match='writer'):
        module.load_manifest(task_manifest(tmp_path, [value]))


def test_batch_accepts_mixed_provider_writer_when_non_overlapping(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    module = load_module()
    value = task(tmp_path, 'writer', source_writing=True)
    value['non_overlapping'] = True
    value['provider_family'] = 'gemini'
    loaded = module.load_manifest(task_manifest(tmp_path, [value]))
    assert loaded[0]['provider_family'] == 'gemini'


def test_batch_cancellation_reaps_active_dispatches(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'cancel')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))
    manifest = task_manifest(tmp_path, [task(tmp_path, f'task-{i}', sleep='5') for i in range(4)])
    parsed = args(module, run_dir, manifest, 2)
    result = []
    import threading
    worker = threading.Thread(target=lambda: result.append(module.batch(parsed)))
    worker.start()
    deadline = time.time() + 3
    while not (tmp_path / 'active').exists() and time.time() < deadline:
        time.sleep(0.01)
    module.request_cancel()
    worker.join(3)
    assert not worker.is_alive()
    assert result == [1]
    assert int((tmp_path / 'active').read_text()) == 0
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    assert summary['status'] == 'cancelled'
