"""Executable contract tests for fixed bounded ordinary batches."""

from __future__ import annotations

import importlib.util
import inspect
import json
import os
import stat
import subprocess
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
BATCH = ROOT / "skills/orchestrate/scripts/batch_run.py"
INIT = ROOT / "skills/orchestrate/scripts/run_dir_init.sh"
FINALIZE = ROOT / "skills/orchestrate/scripts/run_dir_finalize.py"


def write_executable(path: Path, body: str) -> None:
    normalized = inspect.cleandoc(body)
    if normalized.lstrip().startswith("#!"):
        assert normalized.startswith("#!"), "executable fixture shebang must start at byte zero"
    path.write_text(normalized, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


def make_run(tmp_path: Path, name: str) -> Path:
    return Path(subprocess.check_output([str(INIT), str(tmp_path / ".agent-run" / name)], text=True).strip())


def attempt_diagnostics(run_dir: Path) -> str:
    evidence = []
    for name in ("adapter-receipt.json", "result.md", "stderr.log"):
        for path in sorted((run_dir / "dispatch/tasks").glob(f"*/attempt-*/{name}")):
            evidence.append(f"{path.relative_to(run_dir)}:\n{path.read_text(encoding='utf-8')}")
    return "\n".join(evidence)


def load_module():
    spec = importlib.util.spec_from_file_location("batch_run_under_test", BATCH)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def fake_dispatch(path: Path) -> None:
    write_executable(path, """
        #!/usr/bin/env python3
        import argparse, fcntl, hashlib, json, os, pathlib, shutil, signal, time
        p = argparse.ArgumentParser()
        p.add_argument('--task-id', required=True)
        p.add_argument('--prompt-file', required=True)
        p.add_argument('--run-dir', required=True)
        p.add_argument('--timeout')
        p.add_argument('--out')
        p.add_argument('--adapter')
        p.add_argument('--role')
        p.add_argument('--alias')
        p.add_argument('--task-class')
        p.add_argument('--model')
        p.add_argument('--orchestrator-family')
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
        requested_status = values.get('status', 'succeeded')
        status = 'failed' if requested_status == 'empty' else requested_status
        outcome = 'result_missing_or_empty' if requested_status == 'empty' else status
        question = {'code': 'needs_input', 'prompt': values['question']} if status == 'blocked' else None
        attempt_dir = pathlib.Path(ns.run_dir) / 'dispatch/tasks' / ns.task_id / 'attempt-001'
        attempt_dir.mkdir(parents=True, exist_ok=True)
        prompt_path = attempt_dir / 'prompt.md'
        shutil.copyfile(ns.prompt_file, prompt_path)
        stderr_path = attempt_dir / 'stderr.log'
        stderr_path.write_text('', encoding='utf-8')
        adapter_path = attempt_dir / 'adapter-receipt.json'
        adapter = {'tool': ns.adapter, 'adapter': ns.adapter, 'execution_intent': ns.intent,
                   'resolved_model': 'fixture-model', 'provider_family': ns.adapter,
                   'model_family': ns.adapter, 'endpoint_provider': ns.adapter,
                   'identity_source': 'fixture', 'status': 'ok', 'exit': 0,
                   'read_only_guarantee': 'none', 'cross_family': False,
                   'certification_eligible': False}
        adapter_path.write_text(json.dumps(adapter) + '\\n', encoding='utf-8')
        result = None
        result_path = attempt_dir / 'result.md'
        if status == 'succeeded':
            result_path.write_text('OK\\n', encoding='utf-8')
            result = {'path': str(result_path.relative_to(ns.run_dir)),
                      'digest': 'sha256:' + hashlib.sha256(result_path.read_bytes()).hexdigest()}
        route = {**adapter, 'adapter_receipt': {'path': str(adapter_path.relative_to(ns.run_dir)),
                'digest': 'sha256:' + hashlib.sha256(adapter_path.read_bytes()).hexdigest()}}
        attempt_rel = str((attempt_dir / 'attempt.json').relative_to(ns.run_dir))
        attempt = {'schema_version': 1, 'record_type': 'dispatch-attempt', 'task_id': ns.task_id, 'attempt_id': 'attempt-001',
                   'attempt_path': attempt_rel, 'status': status, 'outcome': outcome,
                   'finished_at': '2026-08-29T00:00:00Z',
                   'requested_route': {'intent': ns.intent, 'adapter': ns.adapter,
                       'alias': ns.alias or '', 'task_class': ns.task_class or '',
                       'role': ns.role, 'model': ns.model or '', 'effort': '',
                       'orchestrator_family': ns.orchestrator_family or ''},
                   'route': route,
                   'prompt': {'path': str(prompt_path.relative_to(ns.run_dir)),
                              'digest': 'sha256:' + hashlib.sha256(prompt_path.read_bytes()).hexdigest()},
                   'result': result,
                   'stderr': {'path': str(stderr_path.relative_to(ns.run_dir)),
                              'digest': 'sha256:' + hashlib.sha256(stderr_path.read_bytes()).hexdigest()},
                   'attempt_digest_path': str((attempt_dir / 'attempt.sha256').relative_to(ns.run_dir))}
        if question is not None:
            attempt['question'] = question
        attempt_path = attempt_dir / 'attempt.json'
        attempt_path.write_text(json.dumps(attempt, sort_keys=True) + '\\n', encoding='utf-8')
        attempt_digest = 'sha256:' + hashlib.sha256(attempt_path.read_bytes()).hexdigest()
        (attempt_dir / 'attempt.sha256').write_text(attempt_digest + '  attempt.json\\n', encoding='utf-8')
        record = {'schema_version': 1, 'record_type': 'dispatch-attempt',
                  'status': status, 'outcome': outcome,
                  'task_id': ns.task_id, 'attempt_id': 'attempt-001',
                  'attempt_path': attempt_rel, 'attempt_digest': attempt_digest, 'result': result}
        if question is not None:
            record['question'] = question
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
    result = {'id': task_id, 'prompt_file': str(prompt), 'adapter': 'codex', 'alias': 'scout', 'role': 'worker'}
    for key in ('source_writing', 'non_overlapping', 'worktree_isolated', 'provider_family', 'access_mode'):
        if key in values:
            result[key] = values[key]
    return result


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
    tasks = [task(tmp_path, f'task-{i}', sleep='0.03') for i in range(8)]
    tasks[-1].update({'adapter': 'gemini', 'model': 'flash'})
    tasks[-1].pop('alias')
    manifest = task_manifest(tmp_path, tasks)

    assert module.batch(args(module, run_dir, manifest, 3)) == 0
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    assert summary['status'] == 'completed'
    assert len(summary['tasks']) == 8
    assert int((tmp_path / 'maximum').read_text()) <= 3
    assert {entry['task_id'] for entry in summary['reducer_inputs']} == {f'task-{i}' for i in range(8)}
    assert {entry['route']['provider_family'] for entry in summary['tasks']} == {'codex', 'gemini'}


def test_default_concurrency_scales_down_for_small_manifest(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'small-default')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))
    manifest = task_manifest(tmp_path, [task(tmp_path, 'only')])

    parsed = module.parser().parse_args([
        '--run-dir', str(run_dir), '--manifest', str(manifest),
    ])
    assert parsed.concurrency == 4
    assert module.batch(parsed) == 0
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    assert summary['concurrency'] == 1


def test_batch_help_states_read_only_writer_boundary(tmp_path, monkeypatch):
    module = load_module()
    help_text = module.parser().format_help()
    assert 'Read-only tasks may run together' in help_text
    assert 'source_writing is rejected' in help_text
    assert 'partitioned/worktree-isolated' in help_text
    assert 'writers are deferred' in help_text


def test_real_dispatch_children_defer_manifest_race_and_preserve_route_identity(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'real-eight')
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir()
    write_executable(bin_dir / 'codex', """
        #!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"high"}]}]}'
          exit 0
        fi
        cat >/dev/null
        printf 'OK\\n'
        """)
    monkeypatch.setenv('PATH', f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}")
    module = load_module()
    module.DISPATCH_RUN = BATCH.parent / 'dispatch_run.py'
    manifest = task_manifest(tmp_path, [task(tmp_path, f'task-{i}', sleep='0') for i in range(8)])

    assert module.batch(args(module, run_dir, manifest, 4)) == 0, attempt_diagnostics(run_dir)
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    assert {entry['status'] for entry in summary['tasks']} == {'succeeded'}
    assert all(entry['route']['provider_family'] for entry in summary['tasks'])
    lines = (run_dir / 'MANIFEST.md').read_text(encoding='utf-8').splitlines()
    assert sum('dispatch-' + f'task-{i}' in line for i in range(8) for line in lines) >= 8
    finalized = subprocess.run([str(FINALIZE), str(run_dir), '--status', 'failed',
                                '--reason', 'batch test'], text=True,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    assert finalized.returncode == 0, finalized.stderr + finalized.stdout
    assert not (run_dir / 'dispatch/.batch.lock').exists()


def test_retained_non_success_attempt_allows_partial_route_and_question(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'partial-receipt')
    module = load_module()
    prompt = tmp_path / 'question.md'
    prompt.write_text('question\n', encoding='utf-8')
    current = task(tmp_path, 'partial')
    attempt_dir = run_dir / 'dispatch/tasks/partial/attempt-001'
    attempt_dir.mkdir(parents=True)
    attempt_rel = 'dispatch/tasks/partial/attempt-001/attempt.json'
    question = {'code': 'needs_input', 'prompt': 'which source?'}
    attempt = {
        'schema_version': 1, 'record_type': 'dispatch-attempt', 'task_id': 'partial',
        'attempt_id': 'attempt-001', 'attempt_path': attempt_rel,
        'status': 'timed_out', 'outcome': 'timeout',
        'question': question,
        'requested_route': {'intent': 'ordinary', 'adapter': 'codex', 'alias': 'scout',
                            'task_class': '', 'role': 'worker', 'model': '',
                            'effort': '', 'orchestrator_family': ''},
        'route': {'adapter': 'codex'}, 'result': None,
    }
    attempt_path = run_dir / attempt_rel
    attempt_path.write_text(json.dumps(attempt, sort_keys=True) + '\n', encoding='utf-8')
    record = {'schema_version': 1, 'record_type': 'dispatch-attempt',
              'task_id': 'partial', 'attempt_id': 'attempt-001',
              'status': 'timed_out', 'outcome': 'timeout',
              'question': question, 'attempt_path': attempt_rel,
              'attempt_digest': module.digest(attempt_path)}

    compact = module._validate_child_record(current, record, run_dir, -15)
    assert compact['status'] == 'timed_out'
    assert compact['outcome'] == 'timeout'
    assert compact['question'] == question
    assert compact['route'] == {'adapter': 'codex'}

    with pytest.raises(module.BatchInputError, match='child receipt'):
        module._validate_child_record(current, {**record, 'schema_version': 2}, run_dir, -15)

    attempt['requested_route']['alias'] = 'flagship'
    attempt_path.write_text(json.dumps(attempt, sort_keys=True) + '\n', encoding='utf-8')
    record['attempt_digest'] = module.digest(attempt_path)
    with pytest.raises(module.BatchInputError, match='selector'):
        module._validate_child_record(current, record, run_dir, -15)

    attempt['requested_route']['alias'] = 'scout'
    attempt['record_type'] = 'not-a-dispatch-attempt'
    attempt_path.write_text(json.dumps(attempt, sort_keys=True) + '\n', encoding='utf-8')
    record['attempt_digest'] = module.digest(attempt_path)
    with pytest.raises(module.BatchInputError, match='attempt receipt'):
        module._validate_child_record(current, record, run_dir, -15)

    wrong_dir = run_dir / 'dispatch/tasks/other/attempt-001'
    wrong_dir.mkdir(parents=True)
    wrong_rel = 'dispatch/tasks/other/attempt-001/attempt.json'
    attempt['record_type'] = 'dispatch-attempt'
    attempt['attempt_path'] = wrong_rel
    wrong_path = run_dir / wrong_rel
    wrong_path.write_text(json.dumps(attempt, sort_keys=True) + '\n', encoding='utf-8')
    wrong_record = {**record, 'attempt_path': wrong_rel, 'attempt_digest': module.digest(wrong_path)}
    with pytest.raises(module.BatchInputError, match='identity'):
        module._validate_child_record(current, wrong_record, run_dir, -15)


def test_real_dispatch_timeout_retains_typed_non_success_attempt(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'real-timeout')
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir()
    write_executable(bin_dir / 'codex', """
        #!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"high"}]}]}'
          exit 0
        fi
        sleep 10
        """)
    monkeypatch.setenv('PATH', f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}")
    module = load_module()
    module.DISPATCH_RUN = BATCH.parent / 'dispatch_run.py'
    slow_prompt = tmp_path / 'slow.md'
    slow_prompt.write_text('slow\n', encoding='utf-8')
    manifest = task_manifest(tmp_path, [{'id': 'slow', 'prompt_file': str(slow_prompt),
        'adapter': 'codex', 'model': 'gpt-5.6-luna', 'role': 'worker', 'timeout': 0.1}])

    assert module.batch(args(module, run_dir, manifest, 1)) == 1
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    result = summary['tasks'][0]
    assert result['status'] == 'timed_out'
    assert result['outcome'] in {'timeout', 'batch_timeout'}
    assert (run_dir / 'dispatch/tasks/slow/attempt-001/attempt.json').is_file()


def test_real_dispatch_cancellation_reaps_provider_process(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'real-cancel')
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir()
    provider_pid = tmp_path / 'provider.pid'
    write_executable(bin_dir / 'codex', f"""
        #!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{{"models":[{{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{{"effort":"low"}},{{"effort":"medium"}},{{"effort":"high"}}]}}]}}'
          exit 0
        fi
        printf '%s' "$$" > "{provider_pid}"
        sleep 10
        """)
    monkeypatch.setenv('PATH', f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}")
    module = load_module()
    module.DISPATCH_RUN = BATCH.parent / 'dispatch_run.py'
    prompt = tmp_path / 'cancel.md'
    prompt.write_text('cancel\n', encoding='utf-8')
    manifest = task_manifest(tmp_path, [{'id': 'cancel', 'prompt_file': str(prompt),
        'adapter': 'codex', 'model': 'gpt-5.6-luna', 'role': 'worker', 'timeout': 10}])
    parsed = args(module, run_dir, manifest, 1)
    result = []
    import threading
    worker = threading.Thread(target=lambda: result.append(module.batch(parsed)))
    worker.start()
    deadline = time.time() + 5
    while not provider_pid.exists() and time.time() < deadline:
        time.sleep(0.01)
    assert provider_pid.exists()
    module.request_cancel()
    worker.join(5)
    assert not worker.is_alive()
    assert result == [1]
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    assert summary['status'] == 'cancelled'
    assert not (run_dir / 'dispatch/.batch.lock').exists()
    provider = int(provider_pid.read_text())
    with pytest.raises(ProcessLookupError):
        os.kill(provider, 0)


def test_inline_prompt_is_temporary_but_attempt_prompt_is_retained(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'inline')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))
    manifest = task_manifest(tmp_path, [{'id': 'inline', 'prompt': 'inline body\n',
        'adapter': 'gemini', 'model': 'flash', 'role': 'worker'}])

    assert module.batch(args(module, run_dir, manifest, 1)) == 0
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    assert (run_dir / summary['source_manifest']['path']).is_file()
    attempt = run_dir / 'dispatch/tasks/inline/attempt-001'
    assert (attempt / 'prompt.md').read_text() == 'inline body\n'
    assert not (run_dir / 'dispatch/batches/batch-001/prompts/inline.md').exists()


def test_retry_creates_new_attempt_without_replacing_attempt_one(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'retry')
    bin_dir = tmp_path / 'bin'
    bin_dir.mkdir()
    write_executable(bin_dir / 'codex', """
        #!/usr/bin/env bash
        if [ "$1" = "debug" ] && [ "$2" = "models" ]; then
          printf '{"models":[{"slug":"gpt-5.6-luna","supported_reasoning_levels":[{"effort":"low"},{"effort":"medium"},{"effort":"high"}]}]}'
          exit 0
        fi
        cat >/dev/null
        printf 'OK\\n'
        """)
    monkeypatch.setenv('PATH', f"{bin_dir}:{ROOT / 'scripts'}:{os.environ['PATH']}")
    module = load_module()
    module.DISPATCH_RUN = BATCH.parent / 'dispatch_run.py'
    prompt = tmp_path / 'retry.md'
    prompt.write_text('retry\n', encoding='utf-8')
    first_manifest = task_manifest(tmp_path, [{'id': 'retry', 'prompt_file': str(prompt),
        'adapter': 'codex', 'alias': 'workhorse', 'role': 'worker'}])
    assert module.batch(args(module, run_dir, first_manifest, 1)) == 0, attempt_diagnostics(run_dir)
    second_manifest = task_manifest(tmp_path, [{'id': 'retry', 'prompt_file': str(prompt),
        'adapter': 'codex', 'alias': 'workhorse', 'role': 'worker', 'retry_of': 'attempt-001'}])
    assert module.batch(args(module, run_dir, second_manifest, 1)) == 0
    first = run_dir / 'dispatch/tasks/retry/attempt-001/attempt.json'
    second = run_dir / 'dispatch/tasks/retry/attempt-002/attempt.json'
    assert first.is_file() and second.is_file() and first.read_bytes() != second.read_bytes()


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
        'ok': 'succeeded', 'bad': 'failed', 'empty': 'failed', 'slow': 'timed_out',
    }
    assert next(entry for entry in summary['tasks'] if entry['task_id'] == 'empty')['outcome'] == 'result_missing_or_empty'


def test_batch_preserves_blocked_question_for_later_continuation(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'blocked-question')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))
    manifest = task_manifest(tmp_path, [task(
        tmp_path, 'blocked', status='blocked', question='Which source should I use?'
    )])

    assert module.batch(args(module, run_dir, manifest, 1)) == 1
    summary = json.loads((run_dir / 'dispatch/batches/batch-001/summary.json').read_text())
    result = summary['tasks'][0]
    assert result['status'] == 'blocked'
    assert result['question'] == {
        'code': 'needs_input', 'prompt': 'Which source should I use?'
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


def test_batch_custody_lock_rejects_simultaneous_batch(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'locked')
    module = load_module()
    first = module._acquire_batch_lock(run_dir)
    try:
        with pytest.raises(module.BatchInputError, match='another batch'):
            module._acquire_batch_lock(run_dir)
    finally:
        import fcntl
        fcntl.flock(first.fileno(), fcntl.LOCK_UN)
        first.close()


def test_run_finalizer_rejects_active_batch_custody(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'active-finalizer')
    module = load_module()
    lock = module._acquire_batch_lock(run_dir)
    try:
        finalized = subprocess.run([
            str(FINALIZE), str(run_dir), '--status', 'failed', '--reason', 'test'
        ], text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert finalized.returncode == 1
        assert 'batch custody is active' in finalized.stderr
    finally:
        import fcntl
        fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
        lock.close()


def test_batch_custody_rejects_batches_symlink(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'symlinked-batches')
    outside = tmp_path / 'outside'
    outside.mkdir()
    (run_dir / 'dispatch').mkdir()
    (run_dir / 'dispatch/batches').symlink_to(outside, target_is_directory=True)
    module = load_module()
    with pytest.raises(module.BatchInputError):
        module._acquire_batch_lock(run_dir)


def test_batch_rejects_source_writer_when_parallel(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    module = load_module()
    value = task(tmp_path, 'writer', source_writing=True)
    with pytest.raises(module.BatchInputError, match='unsupported'):
        module.load_manifest(task_manifest(tmp_path, [value]), concurrency=2)


def test_batch_rejects_declarative_writer_isolation_claim(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    module = load_module()
    value = task(tmp_path, 'writer', source_writing=True)
    value['non_overlapping'] = True
    value['provider_family'] = 'gemini'
    with pytest.raises(module.BatchInputError, match='isolation declarations'):
        module.load_manifest(task_manifest(tmp_path, [value]), concurrency=1)


def test_batch_rejects_source_writer_in_read_only_release(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    module = load_module()
    value = task(tmp_path, 'writer', source_writing=True)
    with pytest.raises(module.BatchInputError, match='unsupported'):
        module.load_manifest(task_manifest(tmp_path, [value]), concurrency=1)


def test_malformed_prior_attempt_blocks_all_batch_launches(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'malformed-prior')
    prior = run_dir / 'dispatch/tasks/prior/attempt-001'
    prior.mkdir(parents=True)
    (prior / 'attempt.json').write_text('{"record_type":"dispatch-attempt"}\n', encoding='utf-8')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    manifest = task_manifest(tmp_path, [task(tmp_path, 'next')])
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))

    assert module.batch(args(module, run_dir, manifest, 1)) == 2
    assert not (tmp_path / 'active').exists()
    assert not list((run_dir / 'dispatch/batches').glob('batch-*'))


def test_non_v1_prior_attempt_blocks_reentry_before_launch(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'non-v1-prior')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))

    first_manifest = task_manifest(tmp_path, [task(tmp_path, 'prior')])
    assert module.batch(args(module, run_dir, first_manifest, 1)) == 0
    prior_path = run_dir / 'dispatch/tasks/prior/attempt-001/attempt.json'
    prior = json.loads(prior_path.read_text())
    prior['schema_version'] = 2
    prior_path.write_text(json.dumps(prior, sort_keys=True) + '\n', encoding='utf-8')

    second_manifest = task_manifest(tmp_path, [task(tmp_path, 'next')])
    assert module.batch(args(module, run_dir, second_manifest, 1)) == 2
    assert not (run_dir / 'dispatch/tasks/next').exists()
    assert not (run_dir / 'dispatch/batches/batch-002').exists()


def test_tampered_prior_evidence_digest_blocks_reentry_before_launch(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'tampered-prior')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))

    first_manifest = task_manifest(tmp_path, [task(tmp_path, 'prior')])
    assert module.batch(args(module, run_dir, first_manifest, 1)) == 0
    prior_path = run_dir / 'dispatch/tasks/prior/attempt-001/attempt.json'
    prior = json.loads(prior_path.read_text())
    prior['prompt']['digest'] = 'sha256:' + '0' * 64
    prior_path.write_text(json.dumps(prior, sort_keys=True) + '\n', encoding='utf-8')
    sidecar = prior_path.with_name('attempt.sha256')
    sidecar.write_text(f"{module.digest(prior_path)}  attempt.json\n", encoding='utf-8')

    second_manifest = task_manifest(tmp_path, [task(tmp_path, 'next')])
    assert module.batch(args(module, run_dir, second_manifest, 1)) == 2
    assert not (run_dir / 'dispatch/tasks/next').exists()
    assert not (run_dir / 'dispatch/batches/batch-002').exists()


def test_tampered_attempt_sidecar_blocks_reentry_before_launch(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'tampered-sidecar')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))

    first_manifest = task_manifest(tmp_path, [task(tmp_path, 'prior')])
    assert module.batch(args(module, run_dir, first_manifest, 1)) == 0
    sidecar = run_dir / 'dispatch/tasks/prior/attempt-001/attempt.sha256'
    sidecar.write_text('sha256:' + '0' * 64 + '  attempt.json\n', encoding='utf-8')

    second_manifest = task_manifest(tmp_path, [task(tmp_path, 'next')])
    assert module.batch(args(module, run_dir, second_manifest, 1)) == 2
    assert not (run_dir / 'dispatch/tasks/next').exists()


def test_standalone_dispatch_is_blocked_by_batch_custody(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'standalone-blocked')
    prompt = tmp_path / 'prompt.md'
    prompt.write_text('blocked\n', encoding='utf-8')
    batch_module = load_module()
    lock = batch_module._acquire_batch_lock(run_dir)
    try:
        blocked = subprocess.run([
            str(BATCH.parent / 'dispatch_run.py'),
            '--run-dir', str(run_dir), '--task-id', 'blocked', '--adapter', 'codex',
            '--prompt-file', str(prompt), '--alias', 'scout', '--role', 'worker',
        ], cwd=tmp_path, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        assert blocked.returncode == 2
        assert json.loads(blocked.stdout)['status'] == 'run_custody_busy'
    finally:
        import fcntl
        fcntl.flock(lock.fileno(), fcntl.LOCK_UN)
        lock.close()


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


def test_artifact_failure_cleans_unindexed_batch_directory_and_releases_lock(tmp_path, monkeypatch):
    monkeypatch.chdir(tmp_path)
    run_dir = make_run(tmp_path, 'artifact-failure')
    dispatch = tmp_path / 'fake-dispatch'
    fake_dispatch(dispatch)
    module = load_module()
    module.DISPATCH_RUN = dispatch
    counter = tmp_path / 'counter'
    counter.write_text('0')
    monkeypatch.setenv('BATCH_COUNTER', str(counter))
    manifest = task_manifest(tmp_path, [task(tmp_path, 'never-launched')])

    def fail_artifact(*_args, **_kwargs):
        raise OSError('injected artifact failure')

    monkeypatch.setattr(module, 'atomic_write_bytes', fail_artifact)
    with pytest.raises(OSError, match='injected artifact failure'):
        module.batch(args(module, run_dir, manifest, 1))
    assert not list((run_dir / 'dispatch/batches').glob('batch-*'))
    assert not (run_dir / 'dispatch/.batch.lock').exists()
    assert not (tmp_path / 'active').exists()
    reopened = module._acquire_batch_lock(run_dir)
    try:
        assert reopened.fileno() >= 0
    finally:
        import fcntl
        fcntl.flock(reopened.fileno(), fcntl.LOCK_UN)
        reopened.close()
