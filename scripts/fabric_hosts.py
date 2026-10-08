#!/usr/bin/env python3
"""Instance-owned hosts and the fixed, versioned SSH peer boundary."""
from __future__ import annotations

import argparse
import importlib.util
import json
import math
import os
import re
import selectors
import shutil
import signal
import subprocess
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass
from pathlib import Path

PRODUCT_ROOT = Path(__file__).resolve().parents[1]
for _product_path in (PRODUCT_ROOT / 'scripts', PRODUCT_ROOT / 'skills',
                      PRODUCT_ROOT / 'skills/orchestrate/scripts'):
    if str(_product_path) not in sys.path:
        sys.path.insert(0, str(_product_path))
from _shared.bounded_process import stop_process_group

PROTOCOL_VERSION = 1
MAX_REQUEST_BYTES = 65_536
MAX_RESPONSE_BYTES = 1_048_576
NAME = re.compile(r"[a-z0-9-]+")
DESTINATION = re.compile(r"(?:[A-Za-z0-9_][A-Za-z0-9_.-]*@)?[A-Za-z0-9][A-Za-z0-9_.-]*")
CANCELLED = threading.Event()
ACTIVE_SSH = set()
SSH_LOCK = threading.RLock()
MAX_JSON_DEPTH = 64
MAX_STDERR_BYTES = 4096

PEER_COMMAND = re.compile(r"(?:/|~/)?[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)* peer")


class HostError(ValueError):
    def __init__(self, code, message):
        self.code = code
        super().__init__(message)

    def wire(self):
        return {'code': self.code, 'message': str(self)}

def cancel_checks(_signal=None, _frame=None):
    if CANCELLED.is_set():
        return
    CANCELLED.set()
    with SSH_LOCK:
        children = list(ACTIVE_SSH)
    for child in children:
        stop_process_group(child)

def strict_json(raw):
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError('duplicate member')
            result[key] = value
        return result
    def constant(_value):
        raise ValueError('non-finite JSON value')
    def finite_float(value):
        parsed = float(value)
        if not math.isfinite(parsed):
            raise ValueError('non-finite JSON value')
        return parsed
    if isinstance(raw, (bytes, bytearray)):
        raw = raw.decode('utf-8')
    depth = 0
    quoted = escaped = False
    for char in raw:
        if quoted:
            if escaped:
                escaped = False
            elif char == '\\':
                escaped = True
            elif char == '"':
                quoted = False
        elif char == '"':
            quoted = True
        elif char in '[{':
            depth += 1
            if depth > MAX_JSON_DEPTH:
                raise ValueError('JSON nesting limit exceeded')
        elif char in ']}':
            depth -= 1
    return json.loads(raw, object_pairs_hook=pairs, parse_constant=constant, parse_float=finite_float)

def fields(value, allowed, required, code):
    if not isinstance(value, dict) or set(value) - set(allowed) or set(required) - set(value):
        raise HostError(code, 'Invalid or unknown fields')

def host_name(value, code='invalid_identifier'):
    if not isinstance(value, str) or not NAME.fullmatch(value):
        raise HostError(code, 'Host names use lowercase letters, digits and hyphens')
    return value


@dataclass(frozen=True)
class PeerConfig:
    ssh_destination: str
    peer_command: str = '.local/bin/provenant peer'
    connect_timeout: int = 5
    response_deadline: float = 15


@dataclass(frozen=True)
class HostsConfig:
    local_host: str = 'local'
    peers: dict[str, PeerConfig] = None

    def __post_init__(self):
        if self.peers is None:
            object.__setattr__(self, 'peers', {})

def config_path():
    root = Path(os.environ.get('AGENT_FABRIC_INSTANCE_ROOT') or '~/.agents').expanduser()
    if not root.is_absolute():
        raise HostError('invalid_config', 'Instance root must be absolute')
    return root / '.agent-fabric/hosts.json'

def load_config(path=None):
    path = Path(path) if path is not None else config_path()
    try:
        raw = path.read_bytes()
    except FileNotFoundError:
        return HostsConfig()
    except OSError as exc:
        raise HostError('invalid_config', 'Cannot read host configuration') from exc
    try:
        value = strict_json(raw)
    except (ValueError, UnicodeError) as exc:
        raise HostError('invalid_config', 'Host configuration must be strict JSON') from exc
    code = 'invalid_config'
    fields(value, {'schema_version', 'local_host', 'peers'}, {'schema_version', 'local_host'}, code)
    if type(value['schema_version']) is not int or value['schema_version'] != 1:
        raise HostError(code, 'Unsupported host schema version')
    local = host_name(value['local_host'], code)
    peers = value.get('peers', {})
    if not isinstance(peers, dict):
        raise HostError(code, 'Peers must be an object keyed by host name')
    parsed = {}
    for name, settings in peers.items():
        host_name(name, code)
        if name == local:
            raise HostError(code, 'Local host cannot also be a peer')
        fields(settings, {'ssh_destination', 'peer_command', 'connect_timeout', 'response_deadline'}, {'ssh_destination'}, code)
        peer = PeerConfig(**settings)
        if (not isinstance(peer.ssh_destination, str) or peer.ssh_destination.startswith('-')
                or not DESTINATION.fullmatch(peer.ssh_destination)):
            raise HostError(code, 'SSH destination must be an alias, optionally user@alias')
        if (not isinstance(peer.peer_command, str) or not PEER_COMMAND.fullmatch(peer.peer_command)
                or any(part.startswith('-') for part in peer.peer_command.split())
                or any(part in {'.', '..'} for part in peer.peer_command.split(' ')[0].split('/'))):
            raise HostError(code, 'Peer command must name an executable followed by peer')
        if type(peer.connect_timeout) is not int or not 0 < peer.connect_timeout <= 86_400:
            raise HostError(code, 'Connect timeout must be a positive integer')
        if type(peer.response_deadline) not in {int, float} or not 0 < peer.response_deadline <= 86_400 or not math.isfinite(peer.response_deadline):
            raise HostError(code, 'Response deadline must be positive and finite')
        parsed[name] = peer
    return HostsConfig(local, parsed)

def hosts_list(config):
    return {'schema': 'fabric.hosts.v1', 'hosts': [
        {'host': config.local_host, 'local': True},
        *[{'host': name, 'local': False, **asdict(peer)} for name, peer in config.peers.items()]]}


def parse_identifier(value, local_host):
    if not isinstance(value, str) or value.count('@') > 1:
        raise HostError('invalid_identifier', 'Expected id or id@host')
    identifier, separator, host = value.partition('@')
    if not identifier or not re.fullmatch(r'[A-Za-z0-9_.:-]+', identifier):
        raise HostError('invalid_identifier', 'Identifier must be nonempty and contain no whitespace')
    return identifier, host_name(host if separator else local_host)

def format_identifier(identifier, host):
    parsed, selected = parse_identifier(f'{identifier}@{host}', host)
    return f'{parsed}@{selected}'

def resolve_selector(selector, records, local_host):
    identifier, host = parse_identifier(selector, local_host)
    matches = [row for row in records if row['id'] == identifier and ('@' not in selector or row['host'] == host)]
    if len({row['host'] for row in matches}) > 1 or len(matches) > 1:
        raise HostError('ambiguous_selector', 'Qualify the selector with its owning host')
    if not matches or ('@' not in selector and matches[0]['host'] != local_host):
        raise HostError('selector_not_found', 'Selector does not identify a local record; qualify its host')
    return matches[0]

def protocol_decision(remote_version):
    compatible = type(remote_version) is int and remote_version == PROTOCOL_VERSION
    return {'compatible': compatible, 'reads_flagged': not compatible, 'writes_allowed': compatible}

def ssh_exchange(peer, request, ssh_program, deadline):
    """Bound memory, time and child lifetime; stdin is one closed JSON document."""
    if peer.ssh_destination.startswith('-') or any(part.startswith('-') for part in peer.peer_command.split()):
        raise HostError('invalid_config', 'SSH destination and peer command arguments cannot start with an option')
    encoded = json.dumps(request, allow_nan=False).encode() + b'\n'
    if len(encoded) > MAX_REQUEST_BYTES:
        raise HostError('request_too_large', 'Request exceeds the byte limit')
    command = [ssh_program, '-o', 'BatchMode=yes', '-o', f'ConnectTimeout={peer.connect_timeout}',
               '--', peer.ssh_destination, peer.peer_command]
    with tempfile.TemporaryFile() as input_file:
        input_file.write(encoded)
        input_file.seek(0)
        try:
            process = subprocess.Popen(command, stdin=input_file, stdout=subprocess.PIPE,
                                       stderr=subprocess.PIPE, start_new_session=True)
        except OSError as exc:
            raise HostError('unreachable', 'SSH could not be started') from exc
        with SSH_LOCK:
            ACTIVE_SSH.add(process)
        output = bytearray()
        stderr_tail = bytearray()
        try:
            if CANCELLED.is_set():
                raise HostError('hosts_cancelled', 'Host check cancelled')
            with selectors.DefaultSelector() as streams:
                streams.register(process.stdout, selectors.EVENT_READ, True)
                streams.register(process.stderr, selectors.EVENT_READ, False)
                while streams.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise HostError('timeout', 'Peer response deadline exceeded')
                    for key, _ in streams.select(remaining):
                        chunk = os.read(key.fileobj.fileno(), 65_536)
                        if not chunk:
                            streams.unregister(key.fileobj)
                        elif key.data:
                            output.extend(chunk)
                            if len(output) > MAX_RESPONSE_BYTES:
                                raise HostError('bad_response', 'Peer response exceeds the byte limit')
                        else:
                            stderr_tail.extend(chunk)
                            if len(stderr_tail) > MAX_STDERR_BYTES:
                                del stderr_tail[:-MAX_STDERR_BYTES]
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise HostError('timeout', 'Peer response deadline exceeded')
                try:
                    exit_code = process.wait(timeout=remaining)
                except subprocess.TimeoutExpired as exc:
                    raise HostError('timeout', 'Peer response deadline exceeded') from exc
        except BaseException:
            stop_process_group(process)
            raise
        finally:
            with SSH_LOCK:
                ACTIVE_SSH.discard(process)
            process.stdout.close()
            process.stderr.close()
    stderr_text = bytes(stderr_tail).decode('utf-8', errors='ignore').strip()
    diagnostic = f': {stderr_text}' if stderr_text else ''
    if exit_code == 255:
        raise HostError('unreachable', f'SSH could not reach the peer{diagnostic}')
    if exit_code != 0 and not output:
        code = 'peer_command_not_found' if exit_code == 127 else 'peer_command_failed'
        message = 'Peer command was not found' if exit_code == 127 else f'Peer command exited with status {exit_code}'
        raise HostError(code, f'{message}{diagnostic}')
    try:
        response = strict_json(output)
        fields(response, {'protocol_version', 'ok', 'result', 'error'}, {'protocol_version', 'ok'}, 'bad_response')
        if type(response['protocol_version']) is not int or response['protocol_version'] <= 0 or type(response['ok']) is not bool:
            raise ValueError('invalid envelope')
        if response['ok']:
            if exit_code != 0 or set(response) != {'protocol_version', 'ok', 'result'} or not isinstance(response['result'], dict):
                raise ValueError('invalid success')
        else:
            if exit_code == 0 or set(response) != {'protocol_version', 'ok', 'error'}:
                raise ValueError('invalid failure')
            fields(response['error'], {'code', 'message'}, {'code', 'message'}, 'bad_response')
            if not all(isinstance(response['error'][key], str) for key in ('code', 'message')):
                raise ValueError('invalid error')
    except (ValueError, UnicodeError, RecursionError) as exc:
        raise HostError('bad_response', f'Peer returned an invalid JSON response{diagnostic}') from exc
    return response


class PeerClient:
    """One SSH client implementation, shared by CLI and MCP. No global health state."""
    def __init__(self, host, config, *, ssh_program=None):
        self.host = host_name(host)
        self.config = config
        self.ssh_program = ssh_program or os.environ.get('AGENT_FABRIC_SSH_PROGRAM', 'ssh')
        self.contact = None

    def _exchange(self, verb, params, version, deadline):
        return ssh_exchange(self.config, {'protocol_version': version, 'verb': verb, 'params': params},
                            self.ssh_program, deadline)

    def call(self, verb, params=None, *, write=False):
        deadline = time.monotonic() + self.config.response_deadline
        try:
            if self.contact is None:
                response = self._exchange('hello', {}, PROTOCOL_VERSION, deadline)
                if not response['ok']:
                    return {'host': self.host, 'reachability': 'reachable', **response}
                contact = response['result']
                if (contact.get('host') != self.host or type(contact.get('protocol_version')) is not int
                        or contact.get('protocol_version') != response['protocol_version']
                        or not (contact.get('revision') is None or isinstance(contact.get('revision'), str))):
                    raise HostError('bad_response', 'Peer hello identity or version is invalid')
                self.contact = response
            version = self.contact['protocol_version']
            decision = protocol_decision(version)
            if write and not decision['writes_allowed']:
                return {'host': self.host, 'reachability': 'reachable', **envelope(error=HostError('protocol_mismatch', 'Writes require matching protocol versions')), **decision}
            response = self.contact if verb == 'hello' else self._exchange(verb, params or {}, version, deadline)
            if response['protocol_version'] != version:
                raise HostError('bad_response', 'Peer changed protocol version after hello')
            if decision['compatible'] and response['ok'] and verb == 'doctor':
                validate_doctor_result(response['result'], self.host, (params or {}).get('project_path'))
            result = {'host': self.host, 'reachability': 'reachable', **response, **decision}
            if decision['reads_flagged']:
                result['error'] = HostError('protocol_mismatch', 'Read came from a different protocol version').wire()
            return result
        except HostError as exc:
            self.contact = None
            return {'host': self.host, 'reachability': 'unreachable' if exc.code in {'timeout', 'unreachable'} else 'reachable',
                    **envelope(error=exc)}

def hello():
    try:
        revision = subprocess.run(['git', '-C', str(PRODUCT_ROOT), 'rev-parse', 'HEAD'],
                                  capture_output=True, text=True, timeout=2,
                                  env={k: v for k, v in os.environ.items() if not k.startswith('GIT_')})
        revision = revision.stdout.strip() if revision.returncode == 0 else None
    except (OSError, subprocess.SubprocessError):
        revision = None
    return {'protocol_version': PROTOCOL_VERSION, 'revision': revision, 'host': load_config().local_host}

def validate_doctor_result(result, host, project_path=None):
    required = {'protocol_version', 'revision', 'host', 'project', 'adapters', 'fabric_registrations', 'lane_temporary_paths'}
    if (not required <= set(result) or result['host'] != host
            or type(result['protocol_version']) is not int or result['protocol_version'] != PROTOCOL_VERSION
            or not (result['revision'] is None or isinstance(result['revision'], str))):
        raise HostError('bad_response', 'Doctor host or required fields are invalid')
    project = result['project']
    if (not isinstance(project, dict) or not isinstance(project.get('path'), str)
            or type(project.get('present')) is not bool
            or project_path is not None and project['path'] != project_path):
        raise HostError('bad_response', 'Doctor project result is invalid')
    for key in ('adapters', 'fabric_registrations', 'lane_temporary_paths'):
        if not isinstance(result[key], dict):
            raise HostError('bad_response', 'Doctor diagnostic result is invalid')
    for adapter in result['adapters'].values():
        if (not isinstance(adapter, dict) or 'executable' not in adapter
                or adapter['executable'] is not None and not isinstance(adapter['executable'], str)):
            raise HostError('bad_response', 'Doctor executable result is invalid')
        for key, statuses in [('signin', {'usable', 'unusable', 'unknown'}),
                              ('confinement', {'effective', 'degraded', 'unknown'})]:
            if (not isinstance(adapter.get(key), dict) or not isinstance(adapter[key].get('status'), str)
                    or adapter[key]['status'] not in statuses):
                raise HostError('bad_response', 'Doctor adapter status is invalid')
    for registration in result['fabric_registrations'].values():
        if not isinstance(registration, dict) or type(registration.get('registered')) is not bool:
            raise HostError('bad_response', 'Doctor registration result is invalid')
    socket = result['lane_temporary_paths']
    if (type(socket.get('fits')) is not bool or type(socket.get('max_path_bytes')) is not int
            or type(socket.get('limit_bytes')) is not int):
        raise HostError('bad_response', 'Doctor socket path result is invalid')

def project_identity(cwd=None):
    from worktree import primary_root, PolicyError
    path = Path(cwd or Path.cwd()).resolve()
    try:
        path = primary_root(path)
    except PolicyError:
        # Non-Git directories are valid Fabric projects too.
        pass
    try:
        return path.relative_to(Path.home().resolve()).as_posix()
    except ValueError as exc:
        raise HostError('invalid_project_path', 'Project must be inside the user home for peer placement') from exc

def project_directory(project_path):
    if (not isinstance(project_path, str) or not project_path or '\x00' in project_path
            or Path(project_path).is_absolute() or '..' in Path(project_path).parts):
        raise HostError('invalid_project_path', 'Project path must be home-relative without parent traversal')
    home = Path.home().resolve()
    path = (home / project_path).resolve()
    if not path.is_relative_to(home):
        raise HostError('invalid_project_path', 'Project path resolves outside the user home')
    return path

def lane_socket_paths(project):
    # Use the actual v2 attempt layout; these are estimates until a lane is placed.
    temporary = Path(project) / '.agent-run/runs/20260101-0000-dispatch-task-000000/dispatch/tasks/task/attempt-001/tmp'
    paths = [temporary / 'SingletonSocket', temporary / 'claude/SingletonSocket']
    maximum = max(len(os.fsencode(str(path))) for path in paths)
    limit = 103 if sys.platform == 'darwin' else 107
    return {'fits': maximum <= limit, 'max_path_bytes': maximum, 'limit_bytes': limit,
            'estimated': True, 'layout': 'dispatch/tasks/task/attempt-001/tmp'}

def signin_status(adapter, executable):
    """Probe only local CLI status. Never read/return credentials or launch a model."""
    from _shared.bounded_process import run_bounded
    if CANCELLED.is_set():
        return {'status': 'unknown', 'reason': 'check_cancelled'}
    if executable is None:
        return {'status': 'unusable', 'reason': 'executable_missing'}
    commands = {'claude': ['auth', 'status'], 'codex': ['login', 'status'],
                'cursor': ['status', '--format', 'json'], 'kiro': ['whoami', '--format', 'json']}
    if adapter not in commands:
        return {'status': 'unknown', 'reason': 'no_noninteractive_signin_probe'}
    try:
        probe = run_bounded([executable, *commands[adapter]], cwd=Path.home(), timeout_seconds=2,
                            output_limit_bytes=8192, merge_stderr=False)
    except (OSError, ValueError):
        return {'status': 'unknown', 'reason': 'probe_failed'}
    if probe.timed_out or probe.stdout_truncated or probe.stderr_truncated:
        return {'status': 'unknown', 'reason': 'probe_timeout_or_incomplete'}
    # Status output may include account identifiers; expose only the decision.
    stdout, stderr = probe.stdout or '', probe.stderr or ''
    if adapter == 'claude':
        try:
            auth = strict_json(stdout)
            if isinstance(auth, dict) and type(auth.get('loggedIn')) is bool:
                if auth['loggedIn'] and probe.returncode == 0:
                    return {'status': 'usable', 'reason': 'auth_status'}
                if auth['loggedIn'] is False:
                    return {'status': 'unusable', 'reason': 'signed_out_or_keychain_unavailable'}
        except (ValueError, UnicodeError):
            pass
    elif adapter in {'cursor', 'kiro'}:
        try:
            auth = strict_json(stdout)
            if isinstance(auth, dict):
                if adapter == 'cursor' and type(auth.get('isAuthenticated')) is bool:
                    if auth['isAuthenticated'] and probe.returncode == 0:
                        return {'status': 'usable', 'reason': 'auth_status'}
                    if auth['isAuthenticated'] is False:
                        return {'status': 'unusable', 'reason': 'signed_out'}
                if adapter == 'kiro' and 'account' in auth:
                    if isinstance(auth['account'], dict) and auth['account'] and probe.returncode == 0:
                        return {'status': 'usable', 'reason': 'whoami'}
                    if auth['account'] is None:
                        return {'status': 'unusable', 'reason': 'signed_out'}
        except (ValueError, UnicodeError):
            pass
    elif adapter == 'codex':
        if probe.returncode == 0 and re.search(r'(?im)^logged in\b', stdout + '\n' + stderr):
            return {'status': 'usable', 'reason': 'login_status'}
        if re.search(r'(?im)^not logged in\b', stdout + '\n' + stderr):
            return {'status': 'unusable', 'reason': 'signed_out'}
    # A CLI succeeding alone does not prove sign-in, especially a model list.
    return {'status': 'unknown', 'reason': 'status_not_proven'}

def adapter_doctor(name, project):
    from adapters import profile
    from provider_exec import build_plan
    try:
        adapter = profile(name)
    except ValueError:
        return {'executable': None, 'signin': {'status': 'unknown', 'reason': 'adapter_not_implemented'},
                'confinement': {'status': 'unknown', 'reason': 'adapter_not_implemented'}}
    executable = shutil.which(adapter.CLI)
    try:
        plan = build_plan(name, {'model': '', 'trains_on_prompts': False}, '',
                          cwd=str(project), workspace_root=str(project))
        applied = plan['applied']['confinement']
        confinement = {'status': 'effective' if applied != 'none' else 'degraded',
                       'mechanism': applied, 'mode': 'read_only'}
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        confinement = {'status': 'unknown', 'reason': 'planning_unavailable', 'mode': 'read_only'}
    return {'executable': executable, 'signin': signin_status(name, executable), 'confinement': confinement}

def doctor(project_path=None):
    sys.path.insert(0, str(PRODUCT_ROOT / 'skills/orchestrate/scripts'))
    from model_route import catalogue_snapshot
    result = hello()
    if project_path is None:
        project_path = project_identity()
    project = project_directory(project_path)
    result['project'] = {'path': project_path, 'present': project.is_dir()}
    try:
        configured = catalogue_snapshot()['adapters']
    except (OSError, ValueError, KeyError):
        raise HostError('doctor_unavailable', 'Cannot load adapter catalogue') from None
    # A status probe runs in parallel and has its own two-second bound. The OS
    # confinement probe is cached by its existing owner, and never launches a lane.
    with ThreadPoolExecutor(max_workers=max(1, min(len(configured), 8))) as executor:
        probes = {name: executor.submit(adapter_doctor, name, project if project.is_dir() else Path.home())
                  for name in configured}
        result['adapters'] = {name: future.result() for name, future in probes.items()}
    checker = importlib.util.spec_from_file_location('provenant_install_check', PRODUCT_ROOT / 'scripts/check-provenant-install.py')
    module = importlib.util.module_from_spec(checker)
    checker.loader.exec_module(module)
    result['fabric_registrations'] = module.provider_registration_status(Path.home())
    result['lane_temporary_paths'] = lane_socket_paths(project)
    return result

def select_hosts(config, selectors):
    records = [{'id': name, 'host': name} for name in [config.local_host, *config.peers]]
    if not selectors:
        return [row['host'] for row in records]
    selected = []
    for selector in selectors:
        # Bare host names are host selectors; id@host shares the future record parser.
        identifier, host = parse_identifier(selector, config.local_host)
        row = resolve_selector(selector if '@' in selector else format_identifier(identifier, identifier), records, config.local_host)
        if row['host'] not in selected:
            selected.append(row['host'])
    return selected

def hosts_doctor(config, selected):
    project_path = project_identity()
    def probe(host):
        if host == config.local_host:
            try:
                response = envelope(doctor(project_path))
            except HostError as exc:
                response = envelope(error=exc)
            except Exception:
                response = envelope(error=HostError('doctor_unavailable', 'Local host diagnostics failed'))
            return {'host': host, 'reachability': 'reachable', **response, **protocol_decision(PROTOCOL_VERSION)}
        return PeerClient(host, config.peers[host]).call('doctor', {'project_path': project_path})
    with ThreadPoolExecutor(max_workers=max(1, min(len(selected), 8))) as executor:
        rows = list(executor.map(probe, selected))
    return {'schema': 'fabric.hosts.doctor.v1', 'project_path': project_path, 'hosts': rows}

def parse_request(raw):
    if len(raw) > MAX_REQUEST_BYTES:
        raise HostError('request_too_large', 'Request exceeds the byte limit')
    try:
        request = strict_json(raw)
    except (ValueError, UnicodeError) as exc:
        raise HostError('bad_json', 'Expected one JSON request') from exc
    fields(request, {'protocol_version', 'verb', 'params'}, {'protocol_version', 'verb'}, 'invalid_request')
    if type(request['protocol_version']) is not int or request['protocol_version'] <= 0:
        raise HostError('invalid_request', 'Protocol version must be a positive integer')
    verb = request['verb']
    if not isinstance(verb, str):
        raise HostError('invalid_request', 'Verb must be a string')
    if verb not in {'hello', 'doctor'}:
        raise HostError('unknown_verb', 'Unsupported peer verb')
    if verb != 'hello' and request['protocol_version'] != PROTOCOL_VERSION:
        raise HostError('protocol_mismatch', 'Peer protocol version differs')
    params = request.get('params', {})
    fields(params, set() if verb == 'hello' else {'project_path'}, set(), 'invalid_request')
    return verb, params

def envelope(result=None, error=None):
    response = {'protocol_version': PROTOCOL_VERSION, 'ok': error is None}
    response['result' if error is None else 'error'] = result if error is None else error.wire()
    return response

def peer():
    try:
        verb, params = parse_request(sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1))
        result = hello() if verb == 'hello' else doctor(**params)
        response = envelope(result)
    except HostError as exc:
        response = envelope(error=exc)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        response = envelope(error=HostError('doctor_unavailable', 'Host diagnostics could not complete'))
    print(json.dumps(response, allow_nan=False))
    return 0 if response['ok'] else 1

def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if argv == ['peer']:
        return peer()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('command', choices=['hosts'])
    parser.add_argument('action', choices=['list', 'doctor'])
    parser.add_argument('--json', action='store_true')
    parser.add_argument('hosts', nargs='*')
    args = parser.parse_intermixed_args(argv)
    try:
        config = load_config()
        selected = select_hosts(config, args.hosts)
        result = hosts_doctor(config, selected) if args.action == 'doctor' else hosts_list(config)
        if CANCELLED.is_set():
            raise HostError('hosts_cancelled', 'Host check cancelled')
        if args.action == 'list':
            result['hosts'] = [row for row in result['hosts'] if row['host'] in selected]
    except HostError as exc:
        print(json.dumps(envelope(error=exc)))
        return 1
    if args.json:
        print(json.dumps(result))
    else:
        for row in result['hosts']:
            if args.action == 'list':
                print(row['host'] + (' local' if row['local'] else ' ' + row['ssh_destination']))
            else:
                print(row['host'] + ' ' + row['reachability'] + ' ' + json.dumps(row.get('error') or row.get('result'), sort_keys=True))
    return 0


if __name__ == '__main__':
    signal.signal(signal.SIGHUP, cancel_checks)
    signal.signal(signal.SIGTERM, cancel_checks)
    signal.signal(signal.SIGINT, cancel_checks)
    raise SystemExit(main())
