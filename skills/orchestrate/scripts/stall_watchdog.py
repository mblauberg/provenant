"""Observe silent providers and retain recovery evidence; never signal them."""
from __future__ import annotations

from datetime import UTC, datetime
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from _shared.custody import OwnedFileError, atomic_write_contained, open_contained_regular
import secret_scan

IGNORED = {'.git', '.agent-run', '.worktrees', 'node_modules', '.venv', '__pycache__',
           '.ssh', '.aws', '.kube', '.secrets', 'Archives', 'Monero'}
LIMIT = 64 * 1024


def timestamp(value=None):
    return datetime.fromtimestamp(time.time() if value is None else value, UTC).isoformat(timespec='milliseconds').replace('+00:00', 'Z')


def worktree_write(cwd):
    """Bound the scan; an incomplete pass is reported as such, never as inactivity."""
    try:
        latest = Path(cwd).lstat().st_mtime
    except OSError:
        return None, False
    visited, complete = 0, True
    def unreadable(_error):
        nonlocal complete
        complete = False
    deadline = time.monotonic() + 0.05
    for directory, dirs, files in os.walk(cwd, followlinks=False, onerror=unreadable):
        dirs[:] = [name for name in dirs if name not in IGNORED]
        for name in [*dirs, *files]:
            if name in {'watchdog.json', 'wip-checkpoint.json'}:
                continue
            visited += 1
            if visited > 10000 or time.monotonic() > deadline:
                return timestamp(latest) if latest is not None else None, False
            try:
                value = Path(directory, name).lstat().st_mtime
                latest = value if latest is None else max(latest, value)
            except OSError:
                complete = False
    return timestamp(latest) if latest is not None else None, complete


def register_capture(attempt_dir, events_path, stderr_path):
    record = {'events.jsonl': os.path.relpath(events_path, attempt_dir),
              'stderr.log': os.path.relpath(stderr_path, attempt_dir), 'started_at': timestamp()}
    atomic_write_contained(attempt_dir, 'watchdog-streams.json', (json.dumps(record) + '\n').encode(),
                           label='watchdog capture paths')


def capture_paths(attempt_dir):
    data, _ = event_tail(attempt_dir, 'watchdog-streams.json')
    try:
        record = json.loads(data)
        return record if isinstance(record, dict) else {}
    except ValueError:
        return {}


def event_tail(attempt_dir, name):
    if name in {'events.jsonl', 'stderr.log'}:
        active = capture_paths(attempt_dir).get(name)
        if isinstance(active, str):
            name = active
    try:
        fd, _, _ = open_contained_regular(attempt_dir, name, os.O_RDONLY, label='watchdog event')
        with os.fdopen(fd, 'rb') as stream:
            metadata = os.fstat(stream.fileno())
            stream.seek(max(0, metadata.st_size - LIMIT))
            return stream.read(LIMIT), timestamp(metadata.st_mtime) if metadata.st_size else None
    except (OSError, OwnedFileError):
        return b'', None


def observation(attempt_dir, cwd, pid, alive, silent_seconds=0):
    latest, complete = worktree_write(cwd)
    _, event_at = event_tail(attempt_dir, 'events.jsonl')
    _, stderr_at = event_tail(attempt_dir, 'stderr.log')
    return {'schema': 'fabric.watchdog.v1', 'checked_at': timestamp(), 'pid': pid,
            'process_alive': alive, 'silent_seconds': round(silent_seconds, 3),
            'last_event_at': event_at, 'last_output_at': max(filter(None, (event_at, stderr_at)), default=None),
            'last_worktree_write_at': latest,
            'worktree_scan_complete': complete}


def write_observation(attempt_dir, cwd, pid, alive, silent_seconds):
    record = observation(attempt_dir, cwd, pid, alive, silent_seconds)
    atomic_write_contained(attempt_dir, 'watchdog.json', (json.dumps(record) + '\n').encode(), label='watchdog observation')
    return record


def observe_if_silent(attempt_dir, cwd, pid, alive, started_at, seconds):
    times = [event_tail(attempt_dir, name)[1] for name in ('events.jsonl', 'stderr.log')]
    last_output = max((value for value in times if value),
                      default=capture_paths(attempt_dir).get('started_at', started_at))
    if not isinstance(last_output, str):
        return
    silent = time.time() - datetime.fromisoformat(last_output.replace('Z', '+00:00')).timestamp()
    if silent < seconds:
        return
    try:
        data, _ = event_tail(attempt_dir, 'watchdog.json')
        previous = json.loads(data)
        checked = datetime.fromisoformat(previous['checked_at'].replace('Z', '+00:00')).timestamp()
        if time.time() - checked < seconds:
            return
    except (ValueError, KeyError, TypeError):
        pass
    write_observation(attempt_dir, cwd, pid, alive, silent)


def redact(text):
    data = text.encode('utf-8')
    for _, pattern in secret_scan.PATTERNS:
        data = pattern.sub(b'[redacted]', data)
    return data.decode('utf-8', 'replace')


def git_summary(cwd, *, deadline=None):
    deadline = time.monotonic() + 5 if deadline is None else deadline
    environment = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    environment.update(GIT_OPTIONAL_LOCKS='0', GIT_NO_LAZY_FETCH='1')
    results = []
    for args in [('diff', '--stat', '--no-ext-diff', '--no-textconv', '--ignore-submodules', 'HEAD'),
                 ('status', '--short', '--untracked-files=normal', '--ignore-submodules')]:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return 'unavailable: reaper Git time budget exhausted'
        try:
            with tempfile.TemporaryFile() as output:
                result = subprocess.run(['git', '-C', str(cwd), '-c', 'core.fsmonitor=false',
                                         '-c', 'protocol.allow=never', *args],
                                        env=environment, stdout=output, stderr=subprocess.DEVNULL, timeout=min(5, remaining))
                if result.returncode:
                    return 'unavailable: worktree Git summary could not be read'
                output.seek(0)
                results.append(output.read(LIMIT).decode('utf-8', 'replace'))
        except (OSError, subprocess.TimeoutExpired):
            return 'unavailable: worktree Git summary could not be read'
    return redact('\n'.join(results).strip()) or 'No worktree changes.'


def last_tool_call(data):
    def tool(value):
        if isinstance(value, dict):
            kind = str(value.get('type', ''))
            if kind in {'tool_use', 'tool_call', 'function_call', 'command_execution'} or value.get('tool'):
                return value
            for child in value.values():
                found = tool(child)
                if found is not None:
                    return found
        elif isinstance(value, list):
            for child in reversed(value):
                found = tool(child)
                if found is not None:
                    return found
        return None
    for line in reversed(data.splitlines()):
        try:
            found = tool(json.loads(line))
        except (ValueError, RecursionError):
            continue
        if found is not None:
            return redact(json.dumps(found, ensure_ascii=True))[:4096]
    return None


def checkpoint(attempt_dir, cwd, pid, *, include_git=True, git_deadline=None):
    """The caller proves death and holds publication custody before entering."""
    record = observation(attempt_dir, cwd, pid, False)
    data, _ = event_tail(attempt_dir, 'events.jsonl')
    summary = git_summary(cwd, deadline=git_deadline) if include_git else 'Skipped: read_only lane.'
    record.update(schema='fabric.wip.v1', diff_summary=summary,
                  last_tool_call=last_tool_call(data),
                  last_tool_call_scope='last 64 KiB of retained events; null means unavailable',
                  next_step='Review the worktree diff and retained events/result, then resume or re-dispatch this attempt.')
    atomic_write_contained(attempt_dir, 'wip-checkpoint.json', (json.dumps(record, indent=2) + '\n').encode(), label='WIP checkpoint')
    return record
