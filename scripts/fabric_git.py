"""Bounded Git bundles through the fixed peer boundary; no remote Git shell."""
from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import re
import subprocess
import tempfile
from pathlib import Path

CHUNK_BYTES = 32 * 1024
MAX_BUNDLE_BYTES = 64 * 1024 * 1024
GIT_VERBS = {'git-upload', 'git-result', 'git-download'}
WRITER_MODES = {'worktree_write', 'write', 'rw', 'worktree'}


class CodeTransport:
    def __init__(self, federation):
        self.f = federation
        self.h = federation.h
        self.claims = []
        self.w = self.h._load_module('provenant_host_worktree', self.h.PRODUCT_ROOT / 'scripts/worktree.py')

    def fail(self, code, message):
        raise self.h.HostError(code, message)

    def git(self, repo, *args):
        # Never inherit object/config routing, invoke hooks, fetch lazy objects,
        # or honour replace refs while deciding transport identities.
        env = {**self.w.git_environment(), 'GIT_NO_REPLACE_OBJECTS': '1', 'GIT_NO_LAZY_FETCH': '1'}
        runner = self.h._load_module('provenant_bounded_process', self.h.PRODUCT_ROOT / 'skills/_shared/bounded_process.py').run_bounded
        result = runner(['git', '-C', str(repo), '-c', 'core.hooksPath=/dev/null',
            '-c', 'protocol.file.allow=always', *args], env=env, cwd=repo,
            timeout_seconds=60, output_limit_bytes=self.h.MAX_RESPONSE_BYTES, merge_stderr=False)
        if result.returncode or result.timed_out or result.stdout_truncated or result.stderr_truncated:
            self.fail('code_transport_failed', 'Git object transfer or identity check failed')
        return result.stdout.strip()

    def repo(self, project, receiving=False):
        if receiving and project not in self.f.config.projects:
            self.fail('project_not_configured', 'Git transport requires an explicitly configured project')
        repo = self.h.project_directory(project)
        try:
            if self.w.primary_root(repo) != repo or self.w.owning_root(repo) != repo:
                self.fail('invalid_project_path', 'Git transport requires the primary checkout')
        except self.w.PolicyError as exc:
            self.fail('invalid_project_path', str(exc))
        if self.git(repo, 'rev-parse', '--is-shallow-repository') != 'false':
            self.fail('code_transport_unsupported', 'Shallow repositories cannot supply complete bundles')
        if (self.w.common_git_dir(repo) / 'info/grafts').exists():
            self.fail('code_transport_unsupported', 'Git grafts are unsupported')
        return repo

    def oid(self, value):
        if not isinstance(value, str) or not re.fullmatch(r'[0-9a-f]{40}(?:[0-9a-f]{24})?', value):
            self.fail('invalid_revision', 'Pass an exact lowercase commit object ID')
        return value

    def directory(self, project, identifier, receiving=False):
        self.repo(project, receiving)
        identifier = self.f.operation_id({'operation_id': identifier})
        path = self.f.root / 'code' / hashlib.sha256(project.encode()).hexdigest() / identifier
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
        return path

    def metadata(self, value):
        self.h.fields(value, {'base_revision', 'branch', 'worktree', 'size', 'sha256'},
            {'base_revision', 'branch', 'worktree', 'size', 'sha256'}, 'invalid_code_transport')
        self.oid(value['base_revision'])
        branch, name = value['branch'], value['worktree']
        if (not isinstance(branch, str) or not branch or branch.startswith('-') or
                not isinstance(name, str) or name != branch.replace('/', '-') or not self.w.SAFE_NAME.fullmatch(name)):
            self.fail('invalid_code_transport', 'Writer worktree must have its safe branch-derived name')
        if type(value['size']) is not int or not 0 < value['size'] <= MAX_BUNDLE_BYTES:
            self.fail('code_transport_too_large', 'Git bundle limit is 64 MiB')
        if not isinstance(value['sha256'], str) or not re.fullmatch(r'[0-9a-f]{64}', value['sha256']):
            self.fail('invalid_code_transport', 'Bundle checksum must be SHA-256')
        return value

    def write_json(self, path, value):
        with tempfile.NamedTemporaryFile(mode='w', dir=path.parent, delete=False) as file:
            json.dump(value, file)
            temporary = Path(file.name)
        temporary.replace(path)

    def bundle(self, repo, ref, path):
        if not path.exists():
            with tempfile.TemporaryDirectory(dir=path.parent) as scratch:
                temporary = Path(scratch) / 'bundle'
                self.git(repo, 'bundle', 'create', str(temporary), ref)
                if temporary.stat().st_size > MAX_BUNDLE_BYTES:
                    self.fail('code_transport_too_large', 'Git bundle limit is 64 MiB')
                temporary.replace(path)
        size = path.stat().st_size
        if not 0 < size <= MAX_BUNDLE_BYTES:
            self.fail('code_transport_too_large', 'Git bundle limit is 64 MiB')
        return {'size': size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}

    def outgoing(self, project, request, cwd, identifier, host):
        """Freeze one committed input for each writer task; dirty files stay local."""
        clean = dict(request)
        tasks = clean.get('tasks') or [clean]
        bindings = []
        for index, task in enumerate(tasks):
            effective = {**{k: v for k, v in clean.items() if k != 'tasks'}, **task}
            writer = str(effective.get('mode', 'read_only')).lower() in WRITER_MODES
            if not writer:
                continue
            if request.get('session'):
                self.fail('remote_writer_session_unsupported', 'Writer dispatch requires a task, without a named session')
            if not effective.get('worktree'):
                self.fail('worktree_required', 'Pass the chair linked worktree to transport its committed HEAD')
            original = Path(effective['worktree']).expanduser()
            if not original.is_absolute():
                original = Path(cwd) / original
            local = original.resolve()
            repo = self.repo(project)
            if original.is_symlink() or local.parent != repo / '.worktrees' or self.w.primary_root(local) != repo:
                self.fail('worktree_invalid', 'Writer requires a registered canonical linked worktree')
            branch = self.git(local, 'symbolic-ref', '--short', 'HEAD')
            if local.name != branch.replace('/', '-'):
                self.fail('worktree_invalid', 'Worktree name must derive from its branch')
            if len(identifier) > 120:
                self.fail('invalid_operation_id', 'Writer operation IDs are limited to 120 characters')
            transfer_id = identifier + '-' + str(index)
            path = self.directory(project, transfer_id)
            saved = path / 'outgoing.json'
            if saved.exists():
                frozen = json.loads(saved.read_text())
                if frozen['host'] != host:
                    self.fail('operation_conflict', 'Writer operation is already owned by another host')
                meta = frozen['metadata']
                if meta['branch'] != branch or meta['worktree'] != local.name:
                    self.fail('operation_conflict', 'Writer operation already binds another worktree')
            else:
                base = self.oid(self.git(local, 'rev-parse', 'HEAD'))
                ref = f'refs/provenant/hosts/{host}/{transfer_id}/base'
                # Recover a preparation interrupted before metadata publication
                # from its already-pinned ref, without guessing a new base.
                existing = subprocess.run(['git', '-C', str(repo), 'rev-parse', '--verify', ref],
                    env={**self.w.git_environment(), 'GIT_NO_REPLACE_OBJECTS': '1'}, capture_output=True, text=True)
                if existing.returncode == 0:
                    base = self.oid(existing.stdout.strip())
                else:
                    self.git(repo, 'update-ref', ref, base, '0' * len(base))
                meta = self.metadata({'base_revision': base, 'branch': branch, 'worktree': local.name,
                    **self.bundle(repo, ref, path / 'input.bundle')})
                self.write_json(saved, {'host': host, 'operation_id': identifier, 'metadata': meta})
            bindings.append({'index': index, 'transfer_id': transfer_id, **meta})
        if bindings:
            clean['code_transport'] = bindings
        return clean

    def send(self, client, project, bindings):
        for binding in bindings:
            meta = {k: v for k, v in binding.items() if k not in {'index', 'transfer_id'}}
            path = self.directory(project, binding['transfer_id']) / 'input.bundle'
            with path.open('rb') as handle:
                offset = 0
                while chunk := handle.read(CHUNK_BYTES):
                    result = client.call('git-upload', {'project_path': project, 'input': {
                        'operation_id': binding['transfer_id'], 'metadata': meta,
                        'offset': offset, 'data': base64.b64encode(chunk).decode()}}, write=True)
                    if not result.get('ok'):
                        self.fail(result['error']['code'], result['error']['message'])
                    offset += len(chunk)

    def peer(self, verb, project, request):
        if verb == 'git-result':
            self.h.fields(request, {'id'}, {'id'}, 'invalid_request')
            return self.result(project, request['id'])
        allowed = {'operation_id', 'metadata', 'offset', 'data'} if verb == 'git-upload' else {'operation_id', 'offset'}
        self.h.fields(request, allowed, allowed, 'invalid_request')
        identifier = self.f.operation_id(request)
        with self.f.pending_lock('code-' + identifier):
            path = self.directory(project, identifier, receiving=True)
            offset = request['offset']
            if type(offset) is not int or not 0 <= offset <= MAX_BUNDLE_BYTES or offset % CHUNK_BYTES:
                self.fail('invalid_code_transport', 'Invalid bundle chunk offset')
            if verb == 'git-download':
                file = path / 'result.bundle'
                if not file.is_file():
                    self.fail('code_result_missing', 'Verify the writer result before downloading')
                with file.open('rb') as handle:
                    handle.seek(offset)
                    chunk = handle.read(CHUNK_BYTES)
                return {'data': base64.b64encode(chunk).decode(), 'offset': offset}
            meta = self.metadata(request['metadata'])
            saved = path / 'incoming.json'
            if saved.exists() and json.loads(saved.read_text()) != meta:
                self.fail('operation_conflict', 'Transfer ID already binds another committed input')
            if not saved.exists():
                self.write_json(saved, meta)
            try:
                chunk = base64.b64decode(request['data'], validate=True)
            except (ValueError, TypeError):
                self.fail('invalid_code_transport', 'Invalid bundle chunk encoding')
            if not 0 < len(chunk) <= CHUNK_BYTES or offset + len(chunk) > meta['size']:
                self.fail('invalid_code_transport', 'Invalid bundle chunk length')
            file = path / 'input.bundle'
            with file.open('r+b' if file.exists() else 'w+b') as handle:
                handle.seek(0, 2)
                size = handle.tell()
                if offset < size:
                    handle.seek(offset)
                    if handle.read(len(chunk)) != chunk:
                        self.fail('operation_conflict', 'Repeated chunk changed its bytes')
                elif offset == size:
                    handle.write(chunk)
                else:
                    self.fail('invalid_code_transport', 'Bundle chunks must arrive in order')
            return {'status': 'ok', 'offset': offset}

    def prepare(self, project, request, identifier):
        bindings = request.pop('code_transport', [])
        tasks = request.get('tasks') or [request]
        writers = [i for i, task in enumerate(tasks) if str(task.get('mode', request.get('mode', 'read_only'))).lower() in WRITER_MODES]
        if not isinstance(bindings, list) or [b.get('index') for b in bindings if isinstance(b, dict)] != writers:
            self.fail('invalid_code_transport', 'Each writer requires exactly one transported committed input')
        if writers and request.get('session'):
            self.fail('remote_writer_session_unsupported', 'Continue a transported writer by its qualified run ID')
        contexts = []
        for binding in bindings:
            self.h.fields(binding, {'index', 'transfer_id', 'base_revision', 'branch', 'worktree', 'size', 'sha256'},
                {'index', 'transfer_id', 'base_revision', 'branch', 'worktree', 'size', 'sha256'}, 'invalid_code_transport')
            if binding['transfer_id'] != identifier + '-' + str(binding['index']):
                self.fail('invalid_code_transport', 'Writer transfer must bind the dispatch operation')
            meta = self.metadata({k: v for k, v in binding.items() if k not in {'index', 'transfer_id'}})
            repo = self.repo(project, receiving=True)
            path = self.directory(project, binding['transfer_id'], receiving=True)
            if not (path / 'incoming.json').exists() or json.loads((path / 'incoming.json').read_text()) != meta:
                self.fail('code_input_missing', 'Writer input has not been transferred')
            file = path / 'input.bundle'
            if file.stat().st_size != meta['size'] or hashlib.sha256(file.read_bytes()).hexdigest() != meta['sha256']:
                self.fail('code_input_incomplete', 'Writer bundle is incomplete or its checksum differs')
            # Consume only the single advertised base ref; the sender cannot
            # install branch refs or choose any receiver ref namespace.
            advertised = self.git(repo, 'bundle', 'list-heads', str(file)).splitlines()
            if len(advertised) != 1 or advertised[0].split()[0] != meta['base_revision'] or not advertised[0].split()[1].startswith('refs/provenant/hosts/'):
                self.fail('invalid_code_transport', 'Bundle must advertise exactly the bound base commit')
            source = advertised[0].split()[1]
            ref = f'refs/provenant/hosts/{self.f.config.local_host}/{binding["transfer_id"]}/base'
            self.git(repo, 'bundle', 'verify', str(file))
            self.git(repo, 'fetch', '--no-tags', '--no-write-fetch-head', str(file), source + ':' + ref)
            if self.git(repo, 'cat-file', '-t', meta['base_revision']) != 'commit':
                self.fail('invalid_revision', 'Writer base must be a commit')
            target = repo / '.worktrees' / meta['worktree']
            try:
                if target.exists():
                    known = any(context['project_path'] == project and context['worktree_path'] == str(target)
                        and context['branch'] == meta['branch']
                        for record in (self.f.root / 'writer-contexts').glob('*.json')
                        for context in json.loads(record.read_text()))
                    if not known or target.is_symlink() or self.w.primary_root(target) != repo:
                        self.fail('worktree_invalid', 'Existing worktree has no transported writer context')
                    with self.w.hold_writer_lease(target):
                        if (self.git(target, 'symbolic-ref', '--short', 'HEAD') != meta['branch'] or
                                self.git(target, 'rev-parse', 'HEAD') != meta['base_revision'] or self.w.worktree_residue(target)):
                            self.fail('worktree_invalid', 'Existing writer must be clean at the transported base on its bound branch')
                else:
                    self.w.create(argparse.Namespace(repo=repo, name=meta['worktree'], existing_branch=None,
                        new_branch=meta['branch'], detach=None, start_point=meta['base_revision'], no_node_modules=False))
            except self.w.PolicyError as exc:
                self.fail('worktree_invalid', str(exc))
            target = str(target)
            task = tasks[binding['index']]
            task.update(mode='worktree_write', worktree=target)
            task.pop('cwd', None)
            contexts.append({**binding, 'worktree_path': target, 'project_path': project,
                'host': self.f.config.local_host, 'operation_id': identifier})
            self.claim_context(contexts[-1])
            self.write_json(self.state_file('writer-contexts', identifier), contexts)
        return contexts

    def state_file(self, directory, identifier):
        path = self.f.root / directory
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
        return path / (identifier + '.json')

    def context_owner(self, target):
        git_dir = Path(self.git(target, 'rev-parse', '--absolute-git-dir'))
        return self.state_file('writer-owners', hashlib.sha256(str(git_dir).encode()).hexdigest())

    def claim_context(self, context, expected_owner=None, admission_operation=None):
        target = Path(context['worktree_path'])
        try:
            with self.w.hold_writer_lease(target):
                marker = self.context_owner(target)
                if expected_owner is not None and (not marker.exists() or
                        json.loads(marker.read_text()).get('operation_id') != expected_owner):
                    self.fail('writer_context_superseded', 'Continuation requires the current writer context')
                if marker.exists():
                    saved = json.loads(marker.read_text())
                    for prior in {saved['operation_id'], saved.get('admission_operation')} - {None, admission_operation, context['operation_id']}:
                        with self.f.database() as db:
                            operation = db.execute('SELECT result FROM operations WHERE id=?', (prior,)).fetchone()
                        if not operation or not operation[0]:
                            self.fail('worktree_busy', 'Prior writer preparation has not resolved')
                        launch = self.f.root / 'operations' / f'{prior}.json'
                        if launch.exists():
                            identity = json.loads(launch.read_text())
                            rows = self.f.worker('status', context['project_path'],
                                {'ids': [identity['runId']], 'detail': 'full'}).get('runs', [])
                            rows = [row for row in rows if identity['taskId'] == '*' or row.get('task_id') == identity['taskId']]
                            if not rows or any(row.get('state') != 'terminal' or
                                    row.get('attempt', 0) < identity['attempt'] for row in rows):
                                self.fail('worktree_busy', 'Prior writer has not terminalised')
                previous = json.loads(marker.read_text()) if marker.exists() else None
                current = {'operation_id': context['operation_id'],
                    **({'admission_operation': admission_operation} if admission_operation else {})}
                self.write_json(marker, current)
                self.claims.append((target, previous, current))
        except self.w.PolicyError as exc:
            self.fail('worktree_busy', str(exc))

    def rollback_claims(self):
        # A rejected preflight never started a writer. Restore only our own reservation.
        for target, previous, current in reversed(self.claims):
            with self.w.hold_writer_lease(target):
                marker = self.context_owner(target)
                if marker.exists() and json.loads(marker.read_text()) == current:
                    if previous is None:
                        marker.unlink()
                    else:
                        self.write_json(marker, previous)

    def continuation(self, project, verb, request, identifier):
        selector = request.get(verb)
        if not selector:
            return []
        rows = self.f.worker('status', project, {'ids': [selector], 'detail': 'full'}).get('runs', [])
        if request.get('task_id'):
            rows = [row for row in rows if row.get('task_id') == request['task_id']]
        if len(rows) != 1:
            return []  # The local control owner returns its normal selector error.
        row = rows[0]
        explicit = request.get('worktree') or str(request.get('mode', '')).lower() in WRITER_MODES
        if explicit:
            self.fail('remote_writer_override_unsupported', 'Continue the transported worktree; a new writer needs dispatch and code transport')
        if verb == 'handoff' and (request.get('mode') is not None or request.get('cwd') is not None):
            return []  # The local handoff owner will not inherit writer mode.
        if row.get('mode') != 'worktree_write':
            return []
        matches = []
        for file in (self.f.root / 'writer-contexts').glob('*.json'):
            identity_path = self.f.root / 'operations' / file.name
            if not identity_path.exists() or json.loads(identity_path.read_text()).get('runId') != row.get('run_id'):
                continue
            matches.extend(context for context in json.loads(file.read_text())
                if context['project_path'] == project and context['worktree_path'] == row.get('worktree'))
        if len(matches) != 1:
            self.fail('writer_binding_missing', 'Continuation requires one transported writer context')
        context = matches[0]
        if verb == 'resume':
            self.claim_context(context, expected_owner=context['operation_id'], admission_operation=identifier)
            return []  # Same run/worktree context; immutable exports bind each head.
        previous_owner = context['operation_id']
        context = {**context, 'input_operation_id': context.get('input_operation_id', context['operation_id']),
            'operation_id': identifier}
        self.claim_context(context, expected_owner=previous_owner)
        self.write_json(self.state_file('writer-contexts', identifier), [context])
        return [context]

    def result(self, project, selector):
        repo = self.repo(project, receiving=True)
        matches = []
        for file in (self.f.root / 'writer-contexts').glob('*.json'):
            contexts = json.loads(file.read_text())
            identifier = file.stem
            launch = self.f.root / 'operations' / (identifier + '.json')
            if not launch.exists():
                continue
            identity = json.loads(launch.read_text())
            if contexts[0]['project_path'] != project:
                continue
            if selector == identifier:
                matches.append((contexts, identity))
            else:
                if selector == identity['runId']:
                    rows = self.f.worker('status', project, {'ids': [identity['runId']], 'detail': 'full'}).get('runs', [])
                else:
                    rows = self.f.worker('status', project, {'ids': [selector], 'detail': 'full'}).get('runs', [])
                    rows = [row for row in rows if row.get('run_id') == identity['runId']]
                chosen = [c for c in contexts if any(row.get('worktree') == c['worktree_path'] and
                    selector in {row.get('run_id'), row.get('task_id')} for row in rows)]
                if chosen:
                    matches.append((chosen, identity))
        if len(matches) != 1 or len(matches[0][0]) != 1:
            self.fail('code_result_ambiguous', 'Select exactly one writer operation or task')
        context, identity = matches[0][0][0], matches[0][1]
        value = self.f.worker('status', project, {'ids': [identity['runId']], 'detail': 'full'})
        rows = [row for row in value.get('runs', []) if row.get('worktree') == context['worktree_path']]
        if len(rows) != 1 or rows[0].get('state') != 'terminal':
            self.fail('writer_not_terminal', 'Wait for the owning writer attempt to finish')
        target = Path(context['worktree_path'])
        try:
            with self.w.hold_writer_lease(target):
                current = self.context_owner(target)
                if not current.exists() or json.loads(current.read_text()).get('operation_id') != context['operation_id']:
                    frozen = self.state_file('verified-writers', f'{context["operation_id"]}-{context["index"]}')
                    if frozen.exists():
                        return json.loads(frozen.read_text())
                    self.fail('writer_context_superseded', 'Another operation owns this worktree; the earlier result was not verified')
                if self.git(target, 'symbolic-ref', '--short', 'HEAD') != context['branch']:
                    self.fail('writer_verification_failed', 'Writer worktree changed its branch identity')
                head = self.oid(self.git(target, 'rev-parse', 'HEAD'))
                verification = self.w.verify_claim(target, target, head, self.w.common_git_dir(repo), base_revision=context['base_revision'])
                if verification['status'] != 'accepted':
                    self.fail('writer_verification_failed', verification['reason'])
                result_id = hashlib.sha256((context['operation_id'] + ':' + context['transfer_id'] + ':' + head).encode()).hexdigest()
                path = self.directory(project, result_id, receiving=True)
                receipt = {k: context[k] for k in ('project_path', 'operation_id', 'transfer_id', 'base_revision', 'branch', 'worktree', 'host')}
                receipt.update(head_revision=head, clean=True, worktree_path=str(target), result_id=result_id,
                    input_operation_id=context.get('input_operation_id', context['operation_id']))
                saved = path / 'verification.json'
                if saved.exists() and json.loads(saved.read_text()) != receipt:
                    self.fail('writer_result_changed', 'Writer changed after its result was verified')
                ref = f'refs/provenant/hosts/{self.f.config.local_host}/{result_id}/result'
                self.git(repo, 'update-ref', ref, head)
                info = self.bundle(repo, ref, path / 'result.bundle')
                self.write_json(saved, receipt)
                value = {'status': 'ok', 'verification': receipt, **info}
                self.write_json(self.state_file('verified-writers', f'{context["operation_id"]}-{context["index"]}'), value)
                return value
        except self.w.PolicyError as exc:
            self.fail('writer_verification_failed', str(exc))

    def fetch(self, client, project, selector, host):
        response = client.call('git-result', {'project_path': project, 'input': {'id': selector}}, write=True)
        if not response.get('ok'):
            return self.f.rejection(response)
        value = response['result']
        receipt = value.get('verification', {})
        receipt_fields = {'project_path', 'operation_id', 'input_operation_id', 'transfer_id', 'base_revision',
            'head_revision', 'branch', 'worktree', 'worktree_path', 'host', 'clean', 'result_id'}
        self.h.fields(receipt, receipt_fields, receipt_fields, 'bad_response')
        if receipt.get('host') != host or receipt.get('project_path') != project or receipt.get('clean') is not True:
            self.fail('bad_response', 'Writer verification does not bind the requested host and project')
        identifier = self.f.operation_id({'operation_id': receipt.get('operation_id')})
        with self.f.database() as db:
            requested = db.execute('SELECT host FROM writer_dispatches WHERE id=? AND project=?', (selector, project)).fetchone()
        if requested and identifier != selector:
            self.fail('bad_response', 'Writer verification identifies another requested operation')
        transfer = self.f.operation_id({'operation_id': receipt.get('transfer_id')})
        path = self.directory(project, transfer)
        saved = path / 'outgoing.json'
        if not saved.is_file():
            self.fail('writer_binding_missing', 'This chair has no matching pre-dispatch committed input')
        frozen = json.loads(saved.read_text())
        if frozen['host'] != host:
            self.fail('writer_binding_missing', 'Committed input belongs to another host')
        original = frozen['metadata']
        if any(receipt.get(k) != original[k] for k in ('base_revision', 'branch', 'worktree')) or receipt.get('input_operation_id') != frozen['operation_id'] or not transfer.startswith(frozen['operation_id'] + '-'):
            self.fail('bad_response', 'Writer result changed its pre-dispatch binding')
        # Remote absolute paths are informational; the home-relative worktree
        # identity above is the cross-host binding.
        head = self.oid(receipt.get('head_revision'))
        if head == original['base_revision']:
            self.fail('bad_response', 'Writer verification must identify a new commit')
        result_id = hashlib.sha256((identifier + ':' + transfer + ':' + head).encode()).hexdigest()
        if receipt.get('result_id') != result_id:
            self.fail('bad_response', 'Result export does not bind the verified head')
        size = value.get('size')
        checksum = value.get('sha256')
        self.metadata({**original, 'size': size, 'sha256': checksum})
        repo = self.repo(project)
        with tempfile.NamedTemporaryFile(dir=path, suffix='.bundle') as file:
            for offset in range(0, size, CHUNK_BYTES):
                chunk = client.call('git-download', {'project_path': project, 'input': {'operation_id': result_id, 'offset': offset}})
                if not chunk.get('ok'):
                    return self.f.rejection(chunk)
                data = chunk['result']
                try:
                    raw = base64.b64decode(data['data'], validate=True)
                except (KeyError, ValueError, TypeError):
                    self.fail('bad_response', 'Invalid fetched bundle chunk')
                if data.get('offset') != offset or len(raw) != min(CHUNK_BYTES, size - offset):
                    self.fail('bad_response', 'Fetched bundle chunk differs from bound size')
                file.write(raw)
            file.flush()
            if hashlib.sha256(Path(file.name).read_bytes()).hexdigest() != checksum:
                self.fail('bad_response', 'Fetched bundle checksum differs')
            advertised = self.git(repo, 'bundle', 'list-heads', file.name).splitlines()
            source = f'refs/provenant/hosts/{host}/{result_id}/result'
            if advertised != [head + ' ' + source]:
                self.fail('writer_head_mismatch', 'Fetched head differs from executing-host verification')
            ref = f'refs/provenant/hosts/{host}/{result_id}/result'
            self.git(repo, 'bundle', 'verify', file.name)
            self.git(repo, 'bundle', 'unbundle', file.name)
            if self.git(repo, 'cat-file', '-t', head) != 'commit':
                self.fail('bad_response', 'Verified writer head must be a commit')
            self.git(repo, 'merge-base', '--is-ancestor', original['base_revision'], head)
            self.git(repo, 'fetch', '--no-tags', '--no-write-fetch-head', file.name, source + ':' + ref)
            if self.git(repo, 'rev-parse', ref) != head:
                self.fail('writer_head_mismatch', 'Fetched ref differs from verified head')
        return {'status': 'ok', 'ref': ref, 'verification': receipt,
            'digest': f'fetched {head} from {host}; verified clean writer {original["worktree"]}'}
