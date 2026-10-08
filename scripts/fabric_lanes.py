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
from contextlib import contextmanager
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

VERBS = {'dispatch', 'lanes', 'status', 'cancel', 'resume', 'handoff', 'output', 'operation'}
WRITES = {'dispatch', 'cancel', 'resume', 'handoff'}
PATH_FIELDS = {'cwd', 'worktree', 'add_dirs', 'read_roots'}


class LaneFederation:
    def __init__(self, owner):
        self.h = owner
        self.config = owner.load_config()
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
        db.execute('CREATE TABLE IF NOT EXISTS pending (id TEXT PRIMARY KEY, project TEXT, host TEXT, verb TEXT, payload TEXT)')
        try:
            with db:
                yield db
        finally:
            db.close()

    def worker(self, verb, project, request, launch_path=None):
        directory = self.h.project_directory(project)
        if not directory.is_dir():
            raise self.h.HostError('project_missing', 'Peer project directory is missing')
        node = os.environ.get('FABRIC_NODE') or shutil.which('node')
        loader = self.h.PRODUCT_ROOT / 'runtime/fabric/node_modules/tsx/dist/loader.mjs'
        if not loader.is_file():
            loader = self.h.PRODUCT_ROOT / 'node_modules/tsx/dist/loader.mjs'
        if not node or not loader.is_file():
            raise self.h.HostError('lane_owner_unavailable', 'Node or the Fabric loader is missing')
        command = [node, '--import', str(loader), str(self.h.PRODUCT_ROOT / 'runtime/fabric/src/peer-lanes.ts')]
        run_bounded = self.h._load_module('provenant_bounded_process', self.h.PRODUCT_ROOT / 'skills/_shared/bounded_process.py').run_bounded
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        with tempfile.NamedTemporaryFile(mode='w', dir=self.root, suffix='.json') as payload:
            json.dump({'verb': verb, 'input': request, 'launch_path': str(launch_path) if launch_path else None}, payload)
            payload.flush()
            result = run_bounded([*command, payload.name], cwd=directory, timeout_seconds=60,
                output_limit_bytes=self.h.MAX_RESPONSE_BYTES, merge_stderr=False,
                env={**os.environ, 'PROVENANT_HOST_LOCAL_ONLY': '1',
                    'PROVENANT_REMOTE_LANE': '1' if verb in WRITES else '0'})
        if result.timed_out or result.stdout_truncated or result.returncode != 0:
            raise self.h.HostError('lane_owner_failed', 'Local lane owner did not return a complete response')
        try:
            value = self.h.strict_json(result.stdout)
            if not isinstance(value, dict):
                raise ValueError()
            return value
        except ValueError as exc:
            raise self.h.HostError('bad_response', 'Lane owner returned invalid JSON') from exc

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

    def peer_call(self, verb, params):
        project = params['project_path']
        self.h.project_directory(project)
        request = params.get('input', {})
        if not isinstance(request, dict):
            raise self.h.HostError('invalid_request', 'Lane input must be an object')
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
                value = self.worker('status', project, {'ids': [identity['runId']]})
                recovered = [row for row in value.get('runs', []) if (identity['taskId'] == '*' or row.get('task_id') == identity['taskId'])
                    and row.get('attempt', 0) >= identity['attempt']]
                if recovered:
                    return {**value, 'runs': recovered, 'operation_id': identifier}
            return {'status': 'launch_unknown', 'state': 'launch_unknown', 'operation_id': identifier}
        if verb not in WRITES:
            return self.worker(verb, project, request)
        identifier = self.operation_id(request)
        clean = {key: value for key, value in request.items() if key not in {'operation_id', 'host'}}
        if verb in {'dispatch', 'resume', 'handoff'}:
            if clean.get('git_evidence'):
                raise self.h.HostError('remote_field_unavailable', 'Git evidence transfer belongs to the code transport slice')
            clean = self.rewrite_paths(clean, receiving=True)
            clean['wait_seconds'] = 0
            if any(item.get('mode') in {'worktree_write', 'write', 'rw', 'worktree'} or item.get('worktree')
                   for item in [clean, *clean.get('tasks', [])]):
                raise self.h.HostError('remote_writer_unavailable', 'Remote writer transport belongs to slice 3')
        encoded = json.dumps({'project': project, 'verb': verb,
            'digest': hashlib.sha256(json.dumps(clean, sort_keys=True).encode()).hexdigest()}, sort_keys=True)
        operations = self.root / 'operations'
        operations.mkdir(parents=True, exist_ok=True, mode=0o700)
        lock = operations / f'{identifier}.lock'
        with lock.open('a') as handle:
            fcntl.flock(handle, fcntl.LOCK_EX)
            with self.database() as db:
                row = db.execute('SELECT request,result FROM operations WHERE id=?', (identifier,)).fetchone()
                if row:
                    if row[0] != encoded:
                        raise self.h.HostError('operation_conflict', 'Operation ID was already used for another request')
                    return json.loads(row[1]) if row[1] else self.peer_call('operation', {'project_path': project, 'input': {'operation_id': identifier}})
                # Commit before invoking the launch owner. An interrupted operation is
                # reconciled, never blindly relaunched by the receiving host.
                db.execute('INSERT INTO operations VALUES (?,?,NULL)', (identifier, encoded))
            launch_path = operations / f'{identifier}.json'
            try:
                result = self.worker(verb, project, clean, launch_path)
            except (self.h.HostError, OSError, subprocess.SubprocessError) as exc:
                if launch_path.exists():
                    raise self.h.HostError('lane_owner_failed', 'Owner response was lost after launch was recorded') from exc
                # The fixed worker records intent before starting an owner. Once
                # that worker exits without a record, no lane could have started.
                result = {'status': 'rejected', 'error': getattr(exc, 'code', 'lane_owner_unavailable'),
                    'fix': str(exc)}
            result['operation_id'] = identifier
            with self.database() as db:
                db.execute('UPDATE operations SET result=? WHERE id=?', (json.dumps(result), identifier))
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
            if not path.resolve().is_relative_to(Path.home().resolve()):
                raise self.h.HostError('invalid_remote_path', 'Remote prompt files must be inside home')
            path = path.parent.resolve() / path.name
            owner = self.h._load_module('provenant_lane_prompt_reader', self.h.PRODUCT_ROOT / 'skills/orchestrate/scripts/dispatch_run.py')
            try:
                raw = owner.read_prompt_input(path, Path(cwd or Path.cwd()).resolve(), Path(cwd or Path.cwd()).resolve())
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
        # Selectors scan owner history; default reads cap each owner and the merge.
        clean = {**request, 'ids': None, 'wait_seconds': 0, 'include_history': bool(request.get('ids')),
            'limit': None if request.get('ids') else request.get('limit', 20)}
        rows = []
        hosts = []
        owner_omitted = 0
        def probe(host):
            if host == self.config.local_host:
                try:
                    return {'ok': True, 'result': self.worker(verb, project, clean)}
                except self.h.HostError as exc:
                    return self.h.envelope(error=exc)
            return self.h.PeerClient(host, self.config.peers[host]).call(verb, {'project_path': project, 'input': clean})
        names = [self.config.local_host, *self.config.peers]
        with ThreadPoolExecutor(max_workers=min(len(names), 8)) as executor:
            responses = list(executor.map(probe, names))
        for host, response in zip(names, responses):
            observed = time.time()
            success = response.get('ok') and not response.get('reads_flagged') and isinstance(response['result'].get('runs'), list) and response['result'].get('status') == 'ok'
            hosts.append({'host': host, 'local': host == self.config.local_host, 'reachability': 'reachable' if success else 'unreachable',
                **({'error': response.get('error')} if not success else {})})
            with self.database() as db:
                if success:
                    payload = response['result']['runs']
                    owner_omitted += response['result'].get('omitted', 0)
                    db.execute('INSERT OR REPLACE INTO snapshots VALUES (?,?,?,?,?)', (project, host, verb, observed, json.dumps(payload)))
                else:
                    snapshot = db.execute('SELECT observed,payload FROM snapshots WHERE project=? AND host=? AND verb=?', (project, host, verb)).fetchone()
                    payload = json.loads(snapshot[1]) if snapshot else []
                    observed = snapshot[0] if snapshot else observed
            if not success and not payload:
                payload = [{'id': 'host', 'run_id': 'host', 'state': 'unknown', 'status': 'unknown', 'attempt': 0}]
            for row in payload:
                item = self.qualify(row, host)
                item.update({'reachability': 'reachable' if success else 'unreachable', 'observed_at': observed})
                if not success:
                    item.update({'last_known_state': item.get('state'), 'last_known_status': item.get('status'),
                        'state': 'unreachable', 'status': 'unreachable', 'age_seconds': max(0, time.time() - observed), 'pgid_alive': None})
                    item['digest'] = f'unreachable {item["id"]}; last known {item.get("last_known_status") or item.get("last_known_state")}; age {item["age_seconds"]:.0f}s'
                rows.append(item)
        with self.database() as db:
            pending = db.execute('SELECT id,host,verb,payload FROM pending WHERE project=?', (project,)).fetchall()
        for identifier, host, pending_verb, payload in pending:
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
                    response = self.h.PeerClient(host, self.config.peers[host]).call(pending_verb,
                        {'project_path': project, 'input': json.loads(payload)}, write=True)
            if response.get('ok') and response['result'].get('status') != 'launch_unknown':
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
                        raw_item = {**row, 'state': row.get('state', 'running'), 'attempt': row.get('attempt', 1)}
                        item = self.qualify(raw_item, host)
                        item.update({'reachability': 'reachable', 'observed_at': time.time()})
                        rows.append(item)
                        # Recovery is itself a successful observation. Preserve
                        # it even if the next full read cannot reach this host.
                        with self.database() as db:
                            snapshot = db.execute('SELECT payload FROM snapshots WHERE project=? AND host=? AND verb=?', (project, host, verb)).fetchone()
                            payload = json.loads(snapshot[0]) if snapshot else []
                            matches = [existing for existing in payload if lane_key(existing) == lane_key(item)]
                            if not matches or all(newer(item, existing) for existing in matches):
                                payload = [existing for existing in payload if lane_key(existing) != lane_key(item)]
                                payload.append(raw_item)
                            db.execute('INSERT OR REPLACE INTO snapshots VALUES (?,?,?,?,?)', (project, host, verb, time.time(), json.dumps(payload)))
            else:
                error = response.get('error', {}).get('code')
                if not response.get('ok') and error not in {'timeout', 'unreachable', 'bad_response', 'hosts_cancelled', 'lane_owner_failed', 'peer_command_failed'}:
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
            candidates = [row for row in rows if selector in {row.get('id'), row.get('run_id'), row.get('task_id')}
                or '@' not in selector and (any(isinstance(row.get(key), str) and row[key].split('@')[0] == selector for key in ('id', 'run_id', 'task_id'))
                    or row['host'] == self.config.local_host and (selector == row.get('run_dir')
                        or isinstance(row.get('run_path'), str) and (selector == Path(row['run_path']).name
                            or Path(selector).is_absolute() and Path(selector).resolve() == (self.h.project_directory(project) / '.agent-run' / row['run_path']).resolve())))]
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
                    if any(row['host'] == host and row['reachability'] == 'unreachable' for row in hosts):
                        selected.append({'id': selector, 'run_id': selector, 'task_id': None, 'attempt': 0,
                            'host': host, 'state': 'unreachable', 'status': 'unreachable', 'reachability': 'unreachable',
                            'last_known_state': 'unknown', 'last_known_status': None, 'age_seconds': 0,
                            'digest': f'unreachable {selector}; no last known observation'})
                        continue
                raise self.h.HostError('selector_not_found', 'No matching lane')
            if '@' not in selector and candidates[0]['host'] != self.config.local_host:
                raise self.h.HostError('selector_not_found', 'Selector does not identify a local record; qualify its host')
            selected.extend(candidates)
        rows = selected if request.get('ids') else rows
        state = request.get('state')
        if state:
            rows = [row for row in rows if row.get('state') == state or row.get('status') == state or state == 'active' and row.get('state') != 'terminal']
        limit = request.get('limit', 20)
        omitted = max(0, len(rows) - limit) if limit is not None and not request.get('ids') else 0
        if omitted:
            rows = rows[:limit]
        return {'schema': 'fabric.runs.v2' if verb == 'lanes' else 'fabric.status.v2', 'status': 'ok', 'runs': rows, 'hosts': hosts, 'omitted': omitted + owner_omitted}

    def client_call(self, action, cwd, request):
        if os.environ.get('PROVENANT_HOST_LOCAL_ONLY') == '1':
            return {'local': True}
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
            return {'local': True, 'input': {key: value for key, value in normalized.items() if key not in {'host', 'operation_id'}}}
        try:
            project = self.h.project_identity(cwd)
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
            return {'local': True, 'input': clean}
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
            return {'local': True, 'input': {key: value.rsplit('@', 1)[0] if key in {'id', 'resume', 'handoff', 'session'} and isinstance(value, str) and '@' in value else value
                for key, value in request.items() if key not in {'host', 'operation_id'}}}
        if host not in self.config.peers:
            raise self.h.HostError('unknown_host', 'Host is not configured')
        clean = {key: value.rsplit('@', 1)[0] if key in {'id', 'resume', 'handoff', 'session'} and isinstance(value, str) and '@' in value else value
            for key, value in request.items() if key != 'host'}
        if action in {'dispatch', 'resume', 'handoff'}:
            clean = self.rewrite_paths(clean, cwd=cwd)
        identifier = self.operation_id(clean)
        if action in WRITES:
            clean['operation_id'] = identifier
        client = self.h.PeerClient(host, self.config.peers[host])
        # This contact and placement check happen before persisting/sending a
        # mutation. Only an unreachable implicit placement may fall back.
        contact = client.call('hello', write=action in WRITES)
        if not contact.get('ok') or contact.get('reads_flagged'):
            if action == 'dispatch' and not request.get('host') and not (request.get('session') and '@' in request['session']) and contact.get('reachability') == 'unreachable':
                reason = contact['error']['code']
                return {'local': True, 'fallback': {'from_host': host, 'reason': reason},
                    'digest': f'local fallback from {host}: {reason}'}
            return self.rejection(contact)
        if action in WRITES:
            with self.database() as db:
                previous = db.execute('SELECT project,host,verb,payload FROM pending WHERE id=?', (identifier,)).fetchone()
                current = (project, host, action, json.dumps(clean, sort_keys=True))
                if previous and tuple(previous) != current:
                    raise self.h.HostError('operation_conflict', 'Pending operation ID already belongs to another request')
                db.execute('INSERT OR REPLACE INTO pending VALUES (?,?,?,?,?)', (identifier, *current))
        if action in WRITES and previous and (action != 'dispatch' or clean.get('session')):
            response = client.call('operation', {'project_path': project, 'input': {'operation_id': identifier}})
            if response.get('ok') and response['result'].get('status') == 'operation_missing':
                response = self.h.envelope({'status': 'rejected', 'operation_id': identifier,
                    'error': f'{action}_not_sent', 'fix': 'Issue a new operation for the current attempt'})
        else:
            response = client.call(action, {'project_path': project, 'input': clean}, write=action in WRITES)
        if response.get('ok') and not response.get('reads_flagged'):
            if action in WRITES and response['result'].get('status') != 'launch_unknown':
                with self.database() as db:
                    db.execute('DELETE FROM pending WHERE id=?', (identifier,))
            return self.qualify(response['result'], host)
        if action in WRITES and response.get('error', {}).get('code') in {'timeout', 'unreachable', 'bad_response', 'hosts_cancelled', 'lane_owner_failed', 'peer_command_failed'}:
            return {**self.unknown(identifier, host), 'digest': f'launch_unknown {identifier}@{host}; reconcile by operation ID'}
        if action in WRITES:
            with self.database() as db:
                db.execute('DELETE FROM pending WHERE id=?', (identifier,))
        return self.rejection(response)

    @staticmethod
    def rejection(response):
        error = response.get('error') or {'code': 'bad_response', 'message': 'Invalid peer result'}
        return {'status': 'rejected', 'host': response.get('host'), 'error': error['code'], 'fix': error['message']}
