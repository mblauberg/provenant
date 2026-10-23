"""Lane federation over the slice-one peer client; each host keeps ownership."""
from __future__ import annotations

import fcntl
import hashlib
import json
import os
import re
import shutil
import sqlite3
import subprocess
import time
import tempfile
import uuid
from contextlib import contextmanager, nullcontext
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from pathlib import Path

VERBS = {'dispatch', 'lanes', 'status', 'cancel', 'resume', 'handoff', 'output', 'operation'}
WRITES = {'dispatch', 'cancel', 'resume', 'handoff'}
PATH_FIELDS = {'cwd', 'worktree', 'add_dirs', 'read_roots'}
# Peer errors after which a sent mutation may or may not have landed.
TRANSIENT = {'timeout', 'unreachable', 'bad_response', 'hosts_cancelled', 'peer_command_failed'}


class LaneFederation:
    def __init__(self, owner):
        self.h = owner
        self.config_error = None
        try:
            self.config = owner.load_config()
        except owner.HostError as exc:
            if exc.code != 'invalid_config':
                raise
            # A broken placement file must never stop local work; run as a standalone
            # host and report the error with every local dispatch so it stays visible.
            self.config = owner.HostsConfig()
            self.config_error = str(exc)
        self.root = owner.config_path().parent

    @contextmanager
    def database(self):
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        path = self.root / 'hosts-state.sqlite3'
        descriptor = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
        os.fchmod(descriptor, 0o600)
        os.close(descriptor)
        db = sqlite3.connect(path, timeout=10)
        db.execute('PRAGMA secure_delete=ON')
        db.execute('CREATE TABLE IF NOT EXISTS snapshots (project TEXT, host TEXT, verb TEXT, observed REAL, payload TEXT, PRIMARY KEY(project,host,verb))')
        db.execute('CREATE TABLE IF NOT EXISTS operations (id TEXT PRIMARY KEY, request TEXT, result TEXT)')
        db.execute('CREATE TABLE IF NOT EXISTS writer_dispatches (id TEXT PRIMARY KEY, project TEXT, host TEXT, digest TEXT, sent INTEGER)')
        db.execute('CREATE TABLE IF NOT EXISTS writer_history (id TEXT PRIMARY KEY, project TEXT, host TEXT, bindings TEXT, result TEXT)')
        db.execute('CREATE TABLE IF NOT EXISTS pending (id TEXT PRIMARY KEY, project TEXT, host TEXT, verb TEXT, payload TEXT)')
        try:
            with db:
                yield db
        finally:
            db.close()

    def pending_lock(self, identifier, wait=True):
        identifier = self.operation_id({'operation_id': identifier})
        return self.file_lock(self.root / 'pending-locks' / f'{identifier}.lock', wait)

    def operation_lock(self, identifier, wait=True):
        return self.file_lock(self.root / 'operations' / f'{identifier}.lock', wait)

    @staticmethod
    @contextmanager
    def file_lock(path, wait=True):
        path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        descriptor = os.open(path, os.O_CREAT | os.O_RDWR, 0o600)
        with os.fdopen(descriptor, 'a') as handle:
            try:
                fcntl.flock(handle, fcntl.LOCK_EX | (0 if wait else fcntl.LOCK_NB))
            except BlockingIOError:
                yield None
                return
            try:
                yield handle
            finally:
                fcntl.flock(handle, fcntl.LOCK_UN)

    def worker(self, verb, project, request, launch_path=None, lock=None, reader_cwd=None):
        raise self.h.HostError('lane_owner_unavailable', 'The TS Fabric lane owner is retired')

    def operation_id(self, request):
        identifier = request.get('operation_id') or str(uuid.uuid4())
        if not isinstance(identifier, str) or not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9._-]{0,127}', identifier):
            raise self.h.HostError('invalid_operation_id', 'Operation ID must be a bounded simple identifier')
        return identifier

    def session_identifier(self, value):
        name, separator, host = value.rpartition('@')
        if not separator:
            return value, self.config.local_host
        self.h.host_name(host)
        if not name.strip() or len(name) > 128 or any(ord(char) < 32 for char in name):
            raise self.h.HostError('session_invalid', 'Pass a valid named session')
        return name, host

    def code(self):
        module = self.h._load_module('provenant_fabric_git', self.h.PRODUCT_ROOT / 'scripts/fabric_git.py')
        return module.CodeTransport(self)

    def cleanup_unstarted(self, identifier, result, held=None, transport=None):
        """Cleanup is best effort after the prelaunch outcome has been committed."""
        result.pop('cleanup_pending', None)
        result.pop('cleanup_error', None)
        try:
            remaining = (transport or self.code()).release(identifier, held)
            if remaining:
                result['cleanup_pending'] = remaining
        except Exception as exc:  # any cleanup failure is recorded; the outcome stands
            result['cleanup_error'] = str(exc)
        with self.database() as db:
            db.execute('UPDATE operations SET result=? WHERE id=?', (json.dumps(result), identifier))
        return result

    def settle(self, identifier, locked=False, held=None):
        """Resolve an operation whose preparer died before recording a launch.

        The preparer holds the operation lock until it records a result, and
        its worker shares it until a launch is recorded, so a free lock, an
        empty result and no launch file prove no lane started.
        Returns the recorded result, or None while the outcome is unknown.
        """
        with nullcontext(True) if locked else self.operation_lock(identifier, wait=False) as acquired:
            if not acquired:
                return None
            with self.database() as db:
                row = db.execute('SELECT result FROM operations WHERE id=?', (identifier,)).fetchone()
            if not row:
                return None
            if row[0]:
                result = json.loads(row[0])
                if result.get('status') == 'rejected':
                    return self.cleanup_unstarted(identifier, result, held)
                return result
            if (self.root / 'operations' / f'{identifier}.json').exists():
                return None
            result = {'status': 'rejected', 'error': 'launch_not_started', 'operation_id': identifier,
                'fix': 'The peer stopped before launching; retry with a new operation ID'}
            with self.database() as db:
                db.execute('UPDATE operations SET result=? WHERE id=?', (json.dumps(result), identifier))
            return self.cleanup_unstarted(identifier, result, held)

    def peer_call(self, verb, params):
        project = params['project_path']
        self.h.project_directory(project)
        request = params.get('input', {})
        if not isinstance(request, dict):
            raise self.h.HostError('invalid_request', 'Lane input must be an object')
        if verb in {'git-have', 'git-upload', 'git-result', 'git-download'}:
            return self.code().peer(verb, project, request)
        if verb == 'operation':
            identifier = self.operation_id(request)
            with self.database() as db:
                row = db.execute('SELECT request,result FROM operations WHERE id=?', (identifier,)).fetchone()
            if not row:
                return {'status': 'operation_missing', 'operation_id': identifier}
            saved = json.loads(row[0])
            if saved['project'] != project:
                raise self.h.HostError('operation_conflict', 'Operation belongs to another project')
            if row[1]:
                return json.loads(row[1])
            launch = self.root / 'operations' / f'{identifier}.json'
            if launch.is_file():
                identity = self.h.strict_json(launch.read_bytes())
                if saved['verb'] == 'cancel':
                    value = self.worker('status', project, {'ids': list({target['runId'] for target in identity['targets']}), 'detail': 'full'})
                    recovered = []
                    for target in identity['targets']:
                        row = next((row for row in value.get('runs', []) if row.get('task_id') == target['taskId'] and row.get('run_id') == target['runId']), None)
                        if row:
                            attempt = next((attempt for attempt in row.get('attempts', []) if attempt.get('attempt') == target['attempt']), None)
                            attempt = attempt or (row if row.get('attempt') == target['attempt'] else None)
                            if attempt and attempt.get('state') == 'terminal':
                                recovered.append({**row, **attempt, 'run_id': target['runId'], 'task_id': target['taskId']})
                    if len(recovered) == len(identity['targets']) and recovered:
                        result = {**value, 'runs': recovered, 'operation_id': identifier}
                        with self.database() as db:
                            db.execute('UPDATE operations SET result=? WHERE id=?', (json.dumps(result), identifier))
                        return result
                    return {'status': 'launch_unknown', 'state': 'launch_unknown', 'operation_id': identifier}
                value = self.worker('status', project, {'ids': [identifier]})
                recovered = [row for row in value.get('runs', []) if (identity['taskId'] == '*' or row.get('task_id') == identity['taskId'])
                    and (row.get('attempt', 0) >= identity['attempt']
                        or identity['taskId'] == '*' and row.get('attempt') == 0
                        and row.get('state') == 'terminal' and row.get('status') == 'rejected')]
                if recovered:
                    return {**value, 'runs': recovered, 'operation_id': identifier}
                return {'status': 'launch_unknown', 'state': 'launch_unknown', 'operation_id': identifier}
            return self.settle(identifier) or {'status': 'launch_unknown', 'state': 'launch_unknown', 'operation_id': identifier}
        if verb not in WRITES:
            value = self.worker(verb, project, request)
            self.code().cleanup_readers(value.get('runs', []))
            return value
        identifier = self.operation_id(request)
        clean = {key: value for key, value in request.items() if key not in {'operation_id', 'host'}}
        if verb in {'dispatch', 'resume', 'handoff'}:
            if clean.get('git_evidence'):
                raise self.h.HostError('remote_field_unavailable', 'Git evidence transfer belongs to the code transport slice')
            clean = self.rewrite_paths(clean, receiving=True)
            clean['wait_seconds'] = 0

        encoded = json.dumps({'project': project, 'verb': verb,
            'digest': hashlib.sha256(json.dumps(clean, sort_keys=True).encode()).hexdigest()}, sort_keys=True)
        operations = self.root / 'operations'
        with self.operation_lock(identifier, wait=False) as held:
            if not held:
                # Another preparer, or its orphaned worker, still owns this ID.
                return {'status': 'launch_unknown', 'state': 'launch_unknown', 'operation_id': identifier}
            with self.database() as db:
                row = db.execute('SELECT request,result FROM operations WHERE id=?', (identifier,)).fetchone()
            if row:
                if row[0] != encoded:
                    raise self.h.HostError('operation_conflict', 'Operation ID was already used for another request')
                if row[1]:
                    return json.loads(row[1])
                if (operations / f'{identifier}.json').exists():
                    return self.peer_call('operation', {'project_path': project, 'input': {'operation_id': identifier}})
                return self.settle(identifier, locked=True)
            with self.database() as db:
                # Commit before invoking the launch owner. An interrupted operation is
                # reconciled, never blindly relaunched by the receiving host.
                db.execute('INSERT INTO operations VALUES (?,?,NULL)', (identifier, encoded))
            launch_path = operations / f'{identifier}.json'
            transport = self.code()
            preparing_reader = False
            try:
                preparing_reader = verb == 'dispatch' and bool(clean.get('code_transport')) and not transport.writer_tasks(clean)
                contexts = (transport.prepare(project, clean, identifier) if verb == 'dispatch'
                    else transport.continuation(project, verb, clean, identifier) if verb in {'resume', 'handoff'} else [])
                preparing_reader = False
                for context in contexts:
                    if not context.get('read_only'):
                        continue
                    task = clean['tasks'][context['index']] if clean.get('tasks') else clean
                    prompt = task.get('prompt', clean.get('prompt'))
                    if isinstance(prompt, str):
                        task['prompt'] = (f'Remote read-only checkout: {context["worktree_path"]}\n'
                            f'Working directory: {transport.reader_cwd(context)}\n'
                            f'HEAD: {context["base_revision"]}\n'
                            'Use this checkout and HEAD for paths and revision references in the brief.\n\n' + prompt)
                reader = next((c for c in contexts if c.get('read_only')), None)
                result = (self.worker(verb, project, clean, launch_path, held, reader_cwd=str(transport.reader_cwd(reader)))
                    if reader and (verb in {'resume', 'handoff'} or clean.get('session')) else self.worker(verb, project, clean, launch_path, held))
                if contexts:
                    result['code_transport'] = {'writers': [c for c in contexts if not c.get('read_only')],
                        'readers': [c for c in contexts if c.get('read_only')], 'uncommitted_changes_transported': False}
                    result['digest'] = result.get('digest', '') + '\n  committed input only; uncommitted chair changes were not transported'
            except Exception as exc:  # Anything before launch must still release the preparation.
                if launch_path.exists():
                    raise self.h.HostError('lane_owner_failed', 'Owner response was lost after launch was recorded') from exc
                # The fixed worker records intent before starting an owner. Once
                # that worker exits without a record, no lane could have started.
                result = {'status': 'rejected', 'error': getattr(exc, 'code', 'lane_owner_unavailable'),
                    'fix': str(exc) + '; the peer recorded this prelaunch rejection. Repair the cause, then dispatch with a new operation ID'}
                if preparing_reader:
                    result['code_transport_prelaunch_failure'] = True
            if not launch_path.exists():
                if result.get('status') == 'rejected' and 'new operation ID' not in result.get('fix', ''):
                    result['fix'] = result.get('fix', '') + '; repair the cause, then dispatch with a new operation ID'
            result['operation_id'] = identifier
            with self.database() as db:
                db.execute('UPDATE operations SET result=? WHERE id=?', (json.dumps(result), identifier))
            if not launch_path.exists():
                return self.cleanup_unstarted(identifier, result, transport=transport)
            return result

    def rewrite_paths(self, request, receiving=False, cwd=None):
        value = dict(request)
        if 'tasks' in value:
            if not isinstance(value['tasks'], list) or not all(isinstance(task, dict) for task in value['tasks']):
                raise self.h.HostError('invalid_request', 'Tasks must be an array of objects')
            value['tasks'] = [self.rewrite_paths(task, receiving, cwd) for task in value['tasks']]
        if not receiving and value.get('prompt_file'):
            if value.get('prompt') is not None:
                raise self.h.HostError('prompt_conflict', 'Pass prompt or prompt_file')
            path = Path(value.pop('prompt_file')).expanduser()
            if not path.is_absolute():
                path = Path(cwd or Path.cwd()) / path
            path = path.parent.resolve() / path.name
            owner = self.h._load_module('provenant_lane_prompt_reader', self.h.PRODUCT_ROOT / 'skills/orchestrate/scripts/dispatch_preflight.py')
            try:
                raw = owner.read_prompt_input(path, Path(cwd or Path.cwd()).resolve(), Path(cwd or Path.cwd()).resolve(),
                    read_roots=(path.parent,), max_bytes=self.h.MAX_REQUEST_BYTES)
            except owner.PreflightError as exc:
                raise self.h.HostError(exc.code, str(exc)) from exc
            if len(raw) > self.h.MAX_REQUEST_BYTES:
                raise self.h.HostError('request_too_large', 'Remote prompt exceeds request limit')
            value['prompt'] = raw.decode('utf-8')
        for key in PATH_FIELDS & value.keys():
            paths = value[key] if key in {'add_dirs', 'read_roots'} else [value[key]]
            rewritten = []
            for raw in paths:
                if not isinstance(raw, str):
                    raise self.h.HostError('invalid_remote_path', 'Remote paths must be strings')
                if receiving:
                    if not raw.startswith('~/'):
                        raise self.h.HostError('invalid_remote_path', 'Peer paths must be home-relative')
                    path = self.h.project_directory(raw[2:])
                    rewritten.append(str(path))
                else:
                    path = Path(raw).expanduser()
                    if not path.is_absolute():
                        path = Path(cwd or Path.cwd()) / path
                    try:
                        relative = path.resolve().relative_to(Path.home().resolve())
                    except ValueError as exc:
                        raise self.h.HostError('invalid_remote_path', 'Absolute remote paths must be inside home') from exc
                    rewritten.append('~/' + relative.as_posix())
            value[key] = rewritten if key in {'add_dirs', 'read_roots'} else rewritten[0]
        return value

    def qualify(self, result, host):
        value = dict(result)
        if not value.get('run_id') and isinstance(value.get('id'), str) and value['id'].startswith('mcp-'):
            value['run_id'] = value['id']
        value['host'] = host
        for key in ('id', 'run_id', 'task_id'):
            if isinstance(value.get(key), str) and '@' not in value[key]:
                value[key] = self.h.format_identifier(value[key], host)
        if isinstance(value.get('session'), str):
            value['session'] = value['session'] + '@' + host
        if isinstance(value.get('runs'), list):
            value['runs'] = [self.qualify(row, host) for row in value['runs']]
        if isinstance(value.get('digest'), str) and 'next_offset' not in value:
            value['digest'] = value['digest'] + f'\n  host {host}'
        return value

    def unknown(self, identifier, host):
        return {'id': f'{identifier}@{host}', 'run_id': f'{identifier}@{host}',
            'operation_id': identifier, 'host': host, 'status': 'launch_unknown',
            'state': 'launch_unknown', 'attempt': 0, 'task_id': None}

    def reads(self, verb, project, request):
        def lane_key(row):
            return (str(row.get('run_id') or row.get('id') or '').rsplit('@', 1)[0],
                str(row.get('task_id') or '').rsplit('@', 1)[0])
        def newer(candidate, previous):
            attempt, prior_attempt = candidate.get('attempt', 0), previous.get('attempt', 0)
            return attempt > prior_attempt or (attempt == prior_attempt and candidate.get('last_known_state', candidate.get('state')) == 'terminal'
                and previous.get('last_known_state', previous.get('state')) != 'terminal')
        # Owners resolve individual selectors before serialising any history.
        selectors = request.get('ids') or []
        clean = {**request, 'ids': None, 'wait_seconds': 0, 'include_history': bool(selectors),
            'limit': None if request.get('ids') else request.get('limit', 20)}
        rows = []
        hosts = []
        owner_omitted = 0
        selector_matches = {selector: set() for selector in selectors}
        def owner_read(host, params):
            if host == self.config.local_host:
                try:
                    return {'ok': True, 'result': self.worker(verb, project, params)}
                except self.h.HostError as exc:
                    return self.h.envelope(error=exc)
            return self.h.PeerClient(host, self.config.peers[host]).call(verb, {'project_path': project, 'input': params})
        def owner_selectors(host):
            result = []
            for selector in selectors:
                if '@' in selector:
                    name, owner = self.h.parse_identifier(selector, self.config.local_host)
                    if owner == host:
                        result.append((selector, name))
                elif host == self.config.local_host or '/' not in selector:
                    result.append((selector, selector))
            return result
        def probe(host):
            if not selectors:
                return owner_read(host, clean)
            matched, payload = {}, []
            # A missing selector rejects a batched owner read. Query separately
            # so an ordinary miss on one host cannot conceal another match.
            for selector, name in owner_selectors(host):
                response = owner_read(host, {**{key: value for key, value in clean.items() if key != 'state'}, 'ids': [name]})
                if response.get('ok') and not response.get('reads_flagged'):
                    result = response['result']
                    if result.get('error') in {'run_not_found', 'selector_not_found'}:
                        continue
                    if result.get('status') == 'ok' and isinstance(result.get('runs'), list):
                        matched[selector] = [lane_key(row) for row in result['runs']]
                        payload.extend(result['runs'])
                        continue
                return response
            return {'ok': True, 'result': {'status': 'ok', 'runs': payload}, 'selector_matches': matched}
        names = [self.config.local_host, *self.config.peers]
        if selectors:
            for selector in selectors:
                if '@' in selector and self.h.parse_identifier(selector, self.config.local_host)[1] not in names:
                    raise self.h.HostError('unknown_host', 'Identifier belongs to an unconfigured host')
            names = [host for host in names if owner_selectors(host)]
        with ThreadPoolExecutor(max_workers=min(len(names), 8)) as executor:
            responses = list(executor.map(probe, names))
        for host, response in zip(names, responses):
            observed = time.time()
            success = response.get('ok') and not response.get('reads_flagged') and isinstance(response['result'].get('runs'), list) and response['result'].get('status') == 'ok'
            error = response.get('error')
            if not success and not error:
                result = response.get('result') or {}
                error = {'code': result.get('error') or 'lane_read_failed', 'message': result.get('fix') or 'Owner did not return a lane read'}
            reachability = response.get('reachability') or ('unreachable' if error and error.get('code') in {'timeout', 'unreachable'} else 'reachable')
            failed_state = 'unreachable' if reachability == 'unreachable' else 'read_error'
            hosts.append({'host': host, 'local': host == self.config.local_host, 'reachability': reachability,
                **({'error': error} if not success else {})})
            with self.database() as db:
                if success:
                    payload = [{**row, 'observed_at': observed} for row in response['result']['runs']]
                    owner_omitted += response['result'].get('omitted', 0)
                    saved = payload
                    if selectors:
                        snapshot = db.execute('SELECT observed,payload FROM snapshots WHERE project=? AND host=? AND verb=?', (project, host, verb)).fetchone()
                        prior = [{**row, 'observed_at': row.get('observed_at', snapshot[0])} for row in json.loads(snapshot[1])] if snapshot else []
                        keys = {lane_key(row) for row in payload}
                        saved = [row for row in prior if lane_key(row) not in keys] + payload
                    db.execute('INSERT OR REPLACE INTO snapshots VALUES (?,?,?,?,?)', (project, host, verb, observed, json.dumps(saved)))
                    for selector, keys in response.get('selector_matches', {}).items():
                        selector_matches[selector].update((host, *key) for key in keys)
                else:
                    snapshot = db.execute('SELECT observed,payload FROM snapshots WHERE project=? AND host=? AND verb=?', (project, host, verb)).fetchone()
                    payload = json.loads(snapshot[1]) if snapshot else []
                    observed = snapshot[0] if snapshot else observed
            if not success and not payload:
                payload = [{'id': 'host', 'run_id': 'host', 'state': 'unknown', 'status': 'unknown', 'attempt': 0}]
            for row in payload:
                item = self.qualify(row, host)
                row_observed = row.get('observed_at', observed)
                item.update({'reachability': reachability, 'observed_at': row_observed})
                if not success:
                    item.update({'last_known_state': item.get('state'), 'last_known_status': item.get('status'),
                        'state': failed_state, 'status': failed_state, 'error': error,
                        'age_seconds': max(0, time.time() - row_observed), 'pgid_alive': None})
                    item['digest'] = f'{failed_state} {item["id"]}; {error["code"]}; last known {item.get("last_known_status") or item.get("last_known_state")}; age {item["age_seconds"]:.0f}s'
                rows.append(item)
        with self.database() as db:
            pending = db.execute('SELECT id,host,verb,payload FROM pending WHERE project=?', (project,)).fetchall()
        for identifier, host, pending_verb, payload in pending:
            with self.pending_lock(identifier, wait=False) as acquired:
                if not acquired:
                    rows.append(self.unknown(identifier, host))
                    continue
                # The sender may have completed after this read loaded the list.
                with self.database() as db:
                    current = db.execute('SELECT host,verb,payload FROM pending WHERE id=? AND project=?', (identifier, project)).fetchone()
                if not current:
                    continue
                host, pending_verb, payload = current
                if host not in self.config.peers:
                    rows.append(self.unknown(identifier, host))
                    continue
                response = self.h.PeerClient(host, self.config.peers[host]).call('operation', {'project_path': project, 'input': {'operation_id': identifier}})
                if response.get('ok') and response['result'].get('status') == 'operation_missing':
                    pending_request = json.loads(payload)
                    if pending_verb != 'dispatch' or pending_request.get('session'):
                        response = self.h.envelope({'status': 'rejected', 'operation_id': identifier,
                            'error': f'{pending_verb}_not_sent', 'fix': 'Issue a new operation for the current attempt'})
                    else:
                        client = self.h.PeerClient(host, self.config.peers[host])
                        try:
                            if pending_request.get('code_transport'):
                                self.code().send(client, project, pending_request['code_transport'])
                            response = client.call(pending_verb,
                                {'project_path': project, 'input': pending_request}, write=True)
                        except self.h.HostError as exc:
                            response = self.h.envelope(error=exc)
                if response.get('ok') and response['result'].get('status') != 'launch_unknown':
                    self.code().record_result(identifier, response['result'])
                    with self.database() as db:
                        db.execute('DELETE FROM pending WHERE id=?', (identifier,))
                    # Re-read next poll: reconciliation identifies the run even if
                    # the owner's first status document has not appeared yet.
                    result = response['result']
                    if result.get('status') == 'rejected' and not result.get('id') and not result.get('run_id'):
                        result = {**self.unknown(identifier, host), **result, 'state': 'terminal'}
                    recovered = result.get('runs') or [result]
                    for row in recovered:
                        if row.get('id') or row.get('run_id'):
                            raw_item = {**row, 'state': row.get('state', 'running'), 'attempt': row.get('attempt', 1), 'observed_at': time.time()}
                            item = self.qualify(raw_item, host)
                            item.update({'reachability': 'reachable', 'observed_at': time.time()})
                            rows.append(item)
                            # Recovery is itself a successful observation. Preserve
                            # it even if the next full read cannot reach this host.
                            with self.database() as db:
                                snapshot = db.execute('SELECT observed,payload FROM snapshots WHERE project=? AND host=? AND verb=?', (project, host, verb)).fetchone()
                                payload = [{**existing, 'observed_at': existing.get('observed_at', snapshot[0])}
                                    for existing in json.loads(snapshot[1])] if snapshot else []
                                matches = [existing for existing in payload if lane_key(existing) == lane_key(item)]
                                if not matches or all(newer(item, existing) for existing in matches):
                                    payload = [existing for existing in payload if lane_key(existing) != lane_key(item)]
                                    payload.append(raw_item)
                                db.execute('INSERT OR REPLACE INTO snapshots VALUES (?,?,?,?,?)', (project, host, verb, time.time(), json.dumps(payload)))
                else:
                    error = response.get('error', {}).get('code')
                    if not response.get('ok') and error not in TRANSIENT | {'lane_owner_failed'}:
                        with self.database() as db:
                            db.execute('DELETE FROM pending WHERE id=?', (identifier,))
                        rows.append({**self.unknown(identifier, host), 'state': 'terminal', 'status': 'rejected', 'error': error})
                    else:
                        rows.append(self.unknown(identifier, host))
        # A reconciliation can recover a lane already present in the snapshot.
        unique = {}
        for row in rows:
            key = (row['host'], *lane_key(row))
            if key not in unique or newer(row, unique[key]):
                unique[key] = row
        rows = list(unique.values())
        selected = []
        for selector in request.get('ids') or []:
            candidates = [row for row in rows if (row['host'], *lane_key(row)) in selector_matches[selector]
                or selector in {row.get('id'), row.get('run_id'), row.get('task_id')}
                or '@' not in selector and (any(isinstance(row.get(key), str) and row[key].split('@')[0] == selector for key in ('id', 'run_id', 'task_id'))
                    or row['host'] == self.config.local_host and (selector == row.get('run_dir')
                        or isinstance(row.get('run_path'), str) and (selector == Path(row['run_path']).name
                            or Path(selector).is_absolute() and Path(selector).resolve() == Path(
                                row.get('run_dir') or self.h.project_directory(project) / '.agent-run' / row['run_path']).resolve())))]
            owners = {row['host'] for row in candidates}
            if len(owners) > 1:
                raise self.h.HostError('ambiguous_selector', 'Selector matches records on multiple hosts')
            if not candidates:
                if '@' not in selector:
                    try:
                        local = self.worker(verb, project, {**clean, 'ids': [selector], 'limit': None})
                    except self.h.HostError:
                        local = {}
                    if local.get('runs'):
                        selected.extend(self.qualify(row, self.config.local_host) for row in local['runs'])
                        continue
                if '@' in selector:
                    _, host = self.h.parse_identifier(selector, self.config.local_host)
                    failure = next((row for row in hosts if row['host'] == host and row.get('error')), None)
                    if failure:
                        failed_state = 'unreachable' if failure['reachability'] == 'unreachable' else 'read_error'
                        selected.append({'id': selector, 'run_id': selector, 'task_id': None, 'attempt': 0,
                            'host': host, 'state': failed_state, 'status': failed_state, 'reachability': failure['reachability'], 'error': failure['error'],
                            'last_known_state': 'unknown', 'last_known_status': None, 'age_seconds': 0,
                            'digest': f'{failed_state} {selector}; {failure["error"]["code"]}; no last known observation'})
                        continue
                raise self.h.HostError('selector_not_found', 'No matching lane')
            if '@' not in selector and candidates[0]['host'] != self.config.local_host:
                raise self.h.HostError('selector_not_found', 'Selector does not identify a local record; qualify its host')
            selected.extend(candidates)
        rows = selected if request.get('ids') else rows
        state = request.get('state')
        if state:
            rows = [row for row in rows if row.get('state') == state or row.get('status') == state or state == 'active' and row.get('state') != 'terminal']
        if not selectors:
            def started(row):
                try:
                    return datetime.fromisoformat(row.get('started_at') or '').timestamp()
                except (TypeError, ValueError, OverflowError):
                    return 0
            failures = {}
            for row in rows:
                if row.get('state') in {'unreachable', 'read_error'}:
                    prior = failures.get(row['host'])
                    if prior is None or started(row) > started(prior):
                        failures[row['host']] = row
            def order(row):
                failed = row.get('state') in {'unreachable', 'read_error'}
                known_state = row.get('last_known_state', row.get('state')) if failed else row.get('state')
                uncertain = row.get('state') == 'launch_unknown' or failed and (known_state != 'terminal' or failures.get(row['host']) is row)
                priority = 2 if uncertain else int(known_state != 'terminal')
                return priority, started(row)
            rows.sort(key=order, reverse=True)
        limit = request.get('limit', 20)
        omitted = max(0, len(rows) - limit) if limit is not None and not request.get('ids') else 0
        if omitted:
            rows = rows[:limit]
        return {'schema': 'fabric.runs.v2' if verb == 'lanes' else 'fabric.status.v2', 'status': 'ok', 'runs': rows, 'hosts': hosts, 'omitted': omitted + owner_omitted}

    def client_call(self, action, cwd, request, *, federated_read=False):
        result = self.placed_call(action, cwd, request, federated_read=federated_read)
        if self.config_error and action in {'lanes', 'status'} and result.get('local'):
            project = self.h.project_identity(cwd)
            result = self.worker(action, project, result.get('input', request))
            result['hosts'] = [{'host': self.config.local_host, 'reachability': 'reachable',
                'error': {'code': 'invalid_config', 'message': self.config_error}}]
            result['digest'] = '\n'.join(row.get('digest') or str(row.get('task_id', 'local lane'))
                for row in result.get('runs', [])) + '\ninvalid_config: ' + self.config_error
        if (self.config_error and action == 'dispatch' and result.get('local') and 'fallback' not in result
                and os.environ.get('PROVENANT_HOST_LOCAL_ONLY') != '1'):
            result = {**result, 'fallback': {'from_host': 'hosts.json', 'reason': 'invalid_config', 'detail': self.config_error},
                'digest': 'local fallback: hosts.json invalid_config: ' + self.config_error}
        return result

    def lift_task_hosts(self, request):
        """A batch runs on one host: lift a uniform per-task host, refuse a mixed one."""
        tasks = request.get('tasks')
        if not isinstance(tasks, list) or not any(isinstance(task, dict) and task.get('host') for task in tasks):
            return request
        hosts = {task.get('host') or request.get('host') if isinstance(task, dict) else None for task in tasks}
        if len(hosts) != 1 or None in hosts:
            raise self.h.HostError('host_conflict', 'A batch runs on one host: pass one host at the top level, '
                'or the same host on every task (saw ' + ', '.join(sorted(str(host) for host in hosts)) + ')')
        return {**request, 'host': hosts.pop(), 'tasks': [{k: v for k, v in task.items() if k != 'host'} for task in tasks]}

    def request_project(self, cwd, request):
        """Place from the task workspace before consulting the caller's directory.

        Linked worktrees map to their primary project. A batch has one project,
        and task paths override its inherited paths just as local dispatch does.
        """
        named_paths = []
        for task in request.get('tasks') or [{}]:
            effective = {**request, **task}
            mode = effective.get('mode', 'read_only')
            writer = str(mode).lower() in {'worktree_write', 'write', 'rw', 'worktree'}
            raw = effective.get('worktree' if writer else 'cwd')
            if not isinstance(raw, str) or not raw:
                continue
            path = Path(raw).expanduser()
            path = path if path.is_absolute() else Path(cwd or Path.cwd()) / path
            try:
                named = self.h.project_identity(path)
            except self.h.HostError:
                # Outside home: path rewriting returns invalid_remote_path.
                continue
            named_paths.append((named, path))
        registered = [name for name, _ in named_paths if name in self.config.projects]
        project = registered[0] if registered else self.h.project_identity(cwd)
        return project, next((path for name, path in named_paths if name != project), None)

    def placed_call(self, action, cwd, request, *, federated_read=False):
        if (os.environ.get('PROVENANT_HOST_LOCAL_ONLY') == '1'
                and not (federated_read and action in {'lanes', 'status'})):
            return {'local': True}
        request = self.lift_task_hosts(request)
        def local_result(value):
            if action == 'fetch':
                raise self.h.HostError('remote_host_required', 'Fetch requires a remote writer identifier qualified with its host')
            if action in WRITES and request.get('operation_id'):
                identifier = self.operation_id(request)
                with self.pending_lock(identifier), self.database() as db:
                    pending = db.execute('SELECT host FROM pending WHERE id=?', (identifier,)).fetchone()
                    writer = db.execute('SELECT host FROM writer_dispatches WHERE id=?', (identifier,)).fetchone()
                    pending = pending or writer
                if pending:
                    raise self.h.HostError('operation_conflict', 'Operation remains owned by ' + pending[0] + '; reconcile it before changing placement')
            return value
        if not self.config.peers:
            explicit = request.get('host')
            if explicit and explicit != self.config.local_host:
                raise self.h.HostError('unknown_host', 'Host is not configured')
            normalized = dict(request)
            for key in ('id', 'resume', 'handoff', 'session', 'task_id'):
                if isinstance(normalized.get(key), str) and '@' in normalized[key]:
                    name, host = self.session_identifier(normalized[key]) if key == 'session' else self.h.parse_identifier(normalized[key], self.config.local_host)
                    if host != self.config.local_host:
                        raise self.h.HostError('unknown_host', 'Identifier belongs to an unconfigured host')
                    normalized[key] = name
            if normalized.get('ids'):
                ids = []
                for selector in normalized['ids']:
                    name, host = self.h.parse_identifier(selector, self.config.local_host)
                    if host != self.config.local_host:
                        raise self.h.HostError('unknown_host', 'Identifier belongs to an unconfigured host')
                    ids.append(name)
                normalized['ids'] = ids
            return local_result({'local': True, 'input': {key: value for key, value in normalized.items() if key not in {'host', 'operation_id'}}})
        try:
            project, unmapped = self.request_project(cwd, request)
        except self.h.HostError as exc:
            selectors = [request.get(key) for key in ('id', 'resume', 'handoff', 'session', 'task_id')] + (request.get('ids') or [])
            if (exc.code != 'invalid_project_path' or request.get('host') not in {None, self.config.local_host}
                    or any(isinstance(value, str) and '@' in value and value.rsplit('@', 1)[1] != self.config.local_host for value in selectors)):
                raise
            # Outside-home projects cannot have a peer placement key. Their
            # ordinary local commands retain the existing owner and paths.
            clean = {key: value.rsplit('@', 1)[0] if key in {'id', 'resume', 'handoff', 'session', 'task_id'} and isinstance(value, str) and '@' in value else value
                for key, value in request.items() if key not in {'host', 'operation_id'}}
            if clean.get('ids'):
                clean['ids'] = [value.rsplit('@', 1)[0] if '@' in value else value for value in clean['ids']]
            return local_result({'local': True, 'input': clean})
        if action in {'lanes', 'status'}:
            return self.reads(action, project, request)
        if action == 'dispatch':
            placement = self.config.projects.get(project, {})
            mode = request.get('mode', 'read_only')
            mode = {'read': 'read_only', 'ro': 'read_only', 'write': 'worktree_write', 'rw': 'worktree_write',
                'worktree': 'worktree_write'}.get(str(mode).lower(), str(mode).lower())
            host = request.get('host') or placement.get('modes', {}).get(mode) or placement.get('default_host') or self.config.local_host
            if request.get('session') and '@' not in request['session'] and not request.get('host'):
                host = self.config.local_host
            if request.get('session') and '@' in request['session']:
                _, owner = self.session_identifier(request['session'])
                if request.get('host') and request['host'] != owner:
                    raise self.h.HostError('host_conflict', 'Session owns its host; drop the placement override')
                host = owner
        else:
            selector = request.get('id') or request.get(action) or request.get('session')
            if not selector:
                raise self.h.HostError('invalid_identifier', 'An owning run or session is required')
            if '@' not in selector:
                # Named sessions remain local unless qualified. Run selectors
                # must refuse collisions rather than selecting the first host.
                if request.get('session'):
                    host = self.config.local_host
                else:
                    try:
                        view = self.reads('lanes', project, {'ids': [selector], 'limit': None})
                        host = view['runs'][0]['host']
                    except self.h.HostError as exc:
                        if exc.code != 'selector_not_found':
                            raise
                        # Preserve every local owner selector form, including
                        # relative run directories and linked-worktree aliases.
                        host = self.config.local_host
            else:
                _, host = self.session_identifier(selector) if request.get('session') else self.h.parse_identifier(selector, self.config.local_host)
        if isinstance(request.get('task_id'), str) and '@' in request['task_id']:
            task, task_host = self.h.parse_identifier(request['task_id'], self.config.local_host)
            if task_host != host:
                raise self.h.HostError('host_conflict', 'Run and task must belong to the same host')
            request = {**request, 'task_id': task}
        if host == self.config.local_host:
            return local_result({'local': True, 'input': {key: value.rsplit('@', 1)[0] if key in {'id', 'resume', 'handoff', 'session'} and isinstance(value, str) and '@' in value else value
                for key, value in request.items() if key not in {'host', 'operation_id'}}})
        if host not in self.config.peers:
            raise self.h.HostError('unknown_host', 'Host is not configured')
        if unmapped:
            raise self.h.HostError('invalid_project_path',
                f'Workspace {unmapped} is not inside a project registered in hosts.json; cannot place it on {host}')
        clean = {key: value.rsplit('@', 1)[0] if key in {'id', 'resume', 'handoff', 'session'} and isinstance(value, str) and '@' in value else value
            for key, value in request.items() if key != 'host'}
        identifier = self.operation_id(clean)
        if action == 'fetch':
            return self.code().fetch(self.h.PeerClient(host, self.config.peers[host]), project, clean['id'], host)
        if action in {'dispatch', 'resume', 'handoff'}:
            clean = self.rewrite_paths(clean, cwd=cwd)
        if action in WRITES:
            clean['operation_id'] = identifier
        client = self.h.PeerClient(host, self.config.peers[host])
        with self.pending_lock(identifier) if action in WRITES else nullcontext(True):
            writer = None
            bound_writer = False
            if action == 'dispatch':
                digest = hashlib.sha256(json.dumps(clean, sort_keys=True).encode()).hexdigest()
                with self.database() as db:
                    writer = db.execute('SELECT project,host,digest,sent FROM writer_dispatches WHERE id=?', (identifier,)).fetchone()
                bound_writer = writer is not None
                if writer and tuple(writer[:3]) != (project, host, digest):
                    raise self.h.HostError('operation_conflict', 'Writer operation already binds another host or request')
                if self.code().transport_tasks(clean) and not writer:
                    with self.database() as db:
                        db.execute('INSERT INTO writer_dispatches VALUES (?,?,?,?,0)', (identifier, project, host, digest))
                    writer = (project, host, digest, 0)
            elif action == 'handoff':
                self.code().record_request(project, host, identifier, action, clean)
            previous = None
            if action in WRITES:
                with self.database() as db:
                    previous = db.execute('SELECT project,host,verb,payload FROM pending WHERE id=?', (identifier,)).fetchone()
                # Code transport bindings are frozen per operation, so a retry
                # compares the request without them.
                stored = json.loads(previous[3]) if previous else None
                if stored:
                    stored.pop('code_transport', None)
                if previous and (tuple(previous[:3]) != (project, host, action)
                        or stored != json.loads(json.dumps(clean))):
                    raise self.h.HostError('operation_conflict', 'Pending operation ID already belongs to another request')
            # This contact and placement check happen before persisting/sending a
            # mutation. Only an unreachable implicit placement may fall back.
            contact = client.call('hello', write=action in WRITES)
            if not contact.get('ok') or contact.get('reads_flagged'):
                if previous or bound_writer:
                    return {**self.unknown(identifier, host), 'digest': f'launch_unknown {identifier}@{host}; reconcile by operation ID'}
                if action == 'dispatch' and not request.get('host') and not (request.get('session') and '@' in request['session']) and contact.get('reachability') == 'unreachable':
                    reason = contact['error']['code']
                    if writer:
                        # Bind the first fallback before returning to the local
                        # owner. A retry must not launch this operation remotely.
                        with self.database() as db:
                            db.execute('UPDATE writer_dispatches SET host=?,sent=1 WHERE id=?',
                                (self.config.local_host, identifier))
                            db.execute('UPDATE writer_history SET host=? WHERE id=?',
                                (self.config.local_host, identifier))
                    return {'local': True, 'fallback': {'from_host': host, 'reason': reason},
                        'digest': f'local fallback from {host}: {reason}'}
                return self.rejection(contact)
            if writer:
                # Bundles are built only after contact, from what the peer
                # already holds; nothing has been sent yet, so a failure here
                # is an ordinary rejection.
                try:
                    clean = self.code().outgoing(project, clean, cwd, identifier, host, client)
                except self.h.HostError as exc:
                    if not bound_writer:
                        # Nothing reached the peer: free the ID for any placement.
                        self.code().retire_outgoing(project, clean, identifier, host)
                        with self.database() as db:
                            db.execute('DELETE FROM writer_dispatches WHERE id=?', (identifier,))
                    if (not request.get('host') and not bound_writer and not self.code().writer_tasks(clean)
                            and exc.code not in {'hosts_cancelled', 'operation_conflict'}):
                        with self.database() as db:
                            db.execute('INSERT OR REPLACE INTO writer_dispatches VALUES (?,?,?,?,1)',
                                (identifier, project, self.config.local_host, digest))
                        return {'local': True, 'input': {k: v for k, v in request.items()
                            if k not in {'host', 'operation_id'}},
                            'fallback': {'from_host': host, 'reason': exc.code},
                            'digest': f'local fallback from {host}: {exc.code}'}
                    if exc.code in TRANSIENT:
                        return {'status': 'rejected', 'host': host, 'error': exc.code,
                            'fix': f'{exc}; nothing was sent to {host}. Repair the cause and retry with the same operation ID, or dispatch with host {self.config.local_host} to run locally'}
                    retry = ('A new operation ID is required' if exc.code == 'operation_conflict'
                        else 'Repair the cause and retry with the same operation ID')
                    raise self.h.HostError(exc.code,
                        f'{exc}; nothing was sent to {host}. {retry}') from exc
                self.code().record_request(project, host, identifier, action, clean)
            if action in WRITES:
                current = (project, host, action, json.dumps(clean, sort_keys=True))
                with self.database() as db:
                    db.execute('INSERT OR REPLACE INTO pending VALUES (?,?,?,?,?)', (identifier, *current))
                    if writer:
                        db.execute('UPDATE writer_dispatches SET sent=1 WHERE id=?', (identifier,))
            if action in WRITES and previous and (action != 'dispatch' or clean.get('session')):
                response = client.call('operation', {'project_path': project, 'input': {'operation_id': identifier}})
                if response.get('ok') and response['result'].get('status') == 'operation_missing':
                    response = self.h.envelope({'status': 'rejected', 'operation_id': identifier,
                        'error': f'{action}_not_sent', 'fix': 'Issue a new operation for the current attempt'})
            else:
                if action == 'dispatch' and clean.get('code_transport'):
                    try:
                        self.code().send(client, project, clean['code_transport'])
                    except self.h.HostError as exc:
                        if (not request.get('host') and not previous and not bound_writer and not request.get('session')
                                and not self.code().writer_tasks(clean)
                                and exc.code not in {'hosts_cancelled', 'operation_conflict'}):
                            # Only chunks were sent: this fresh request has never
                            # invoked the peer launch owner. Bind its local fallback.
                            with self.database() as db:
                                db.execute('DELETE FROM pending WHERE id=?', (identifier,))
                                db.execute('UPDATE writer_dispatches SET host=?,sent=1 WHERE id=?',
                                    (self.config.local_host, identifier))
                                db.execute('UPDATE writer_history SET host=? WHERE id=?',
                                    (self.config.local_host, identifier))
                            self.code().retire_outgoing(project, clean, identifier, host)
                            return {'local': True, 'input': {k: v for k, v in request.items()
                                if k not in {'host', 'operation_id'}},
                                'fallback': {'from_host': host, 'reason': exc.code},
                                'digest': f'local fallback from {host}: {exc.code}'}
                        if exc.code in TRANSIENT:
                            return {**self.unknown(identifier, host), 'digest': f'launch_unknown {identifier}@{host}; retry frozen code transport'}
                        raise
                response = client.call(action, {'project_path': project, 'input': clean}, write=action in WRITES)
            if response.get('ok') and not response.get('reads_flagged'):
                result = response['result']
                if (action == 'dispatch' and not request.get('host') and writer and not previous and not bound_writer
                        and not request.get('session') and result.get('status') == 'rejected'
                        and result.get('code_transport_prelaunch_failure') is True
                        and result.get('error') not in {'hosts_cancelled', 'operation_conflict'}):
                    # The peer durably proved its input preparation never launched.
                    with self.database() as db:
                        db.execute('DELETE FROM pending WHERE id=?', (identifier,))
                        db.execute('UPDATE writer_dispatches SET host=?,sent=1 WHERE id=?',
                            (self.config.local_host, identifier))
                        db.execute('UPDATE writer_history SET host=? WHERE id=?',
                            (self.config.local_host, identifier))
                    self.code().retire_outgoing(project, clean, identifier, host)
                    reason = result['error']
                    return {'local': True, 'input': {k: v for k, v in request.items()
                        if k not in {'host', 'operation_id'}},
                        'fallback': {'from_host': host, 'reason': reason},
                        'digest': f'local fallback from {host}: {reason}'}
                if action in WRITES and response['result'].get('status') != 'launch_unknown':
                    self.code().record_result(identifier, response['result'])
                    with self.database() as db:
                        db.execute('DELETE FROM pending WHERE id=?', (identifier,))
                return self.qualify(response['result'], host)
            if action in WRITES and response.get('error', {}).get('code') in TRANSIENT | {'lane_owner_failed'}:
                return {**self.unknown(identifier, host), 'digest': f'launch_unknown {identifier}@{host}; reconcile by operation ID'}
            if action in WRITES:
                with self.database() as db:
                    db.execute('DELETE FROM pending WHERE id=?', (identifier,))
            return self.rejection(response)

    @staticmethod
    def rejection(response):
        error = response.get('error') or {'code': 'bad_response', 'message': 'Invalid peer result'}
        return {'status': 'rejected', 'host': response.get('host'), 'error': error['code'], 'fix': error['message']}
