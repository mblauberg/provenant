"""Host transport contracts, exercised without an sshd or provider sign-in."""
import json
import os
from pathlib import Path
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from contextlib import contextmanager, closing
from unittest.mock import patch

from tests.support.git import git

from tests.support.front_door import front_door_binary

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = ROOT.parent.parent


def default_host(config=None):
    """The local id a subprocess on this machine uses: a valid local_host, else the hostname's slug."""
    import host_identity
    local = (config or {}).get('local_host')
    return local if isinstance(local, str) and host_identity.NAME.fullmatch(local) else \
        host_identity.slug(host_identity.hostname())


@contextmanager
def git_isolated_tempdir():
    """Keep fixtures in the worktree without inheriting its Git identity."""
    scratch = ROOT / '.agent-run/scratch'
    scratch.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        root = Path(directory)
        # An invalid, existing Git directory stops discovery even when a peer
        # sanitizes PATH and GIT_*; a missing target would be ignored by Git.
        marker = root / 'not-a-git-directory'
        marker.write_text('fixture boundary')
        (root / '.git').write_text('gitdir: ' + str(marker) + '\n')
        yield directory


def commit_file(repo, name, text, msg):
    """Write ``name`` under ``repo``, stage it and commit it with ``msg``."""
    path = Path(repo) / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    git(repo, 'add', name)
    git(repo, 'commit', '-qm', msg)


class PeerContracts(unittest.TestCase):
    def test_lane_invalid_request_reports_bounded_cause_without_exception_message(self):
        import fabric_hosts as hosts
        import io
        cases = [
            (PermissionError(1, 'private exception message', '/denied/state.lock'),
             'PermissionError', '/denied/state.lock'),
            (KeyError('missing_key'), 'KeyError', 'missing_key'),
            (KeyError('k' * 1000 + '\n'), 'KeyError', 'k' * 100),
            (ValueError('private exception message'), 'ValueError', None),
            (UnicodeError('private exception message'), 'UnicodeError', None),
            (TypeError('private exception message'), 'TypeError', None),
        ]
        for error, name, target in cases:
            with self.subTest(name=name, target=target), patch.object(sys, 'stdin') as stdin, \
                    patch.object(sys, 'stdout', new_callable=io.StringIO) as stdout:
                stdin.buffer.read.side_effect = error
                self.assertEqual(hosts.main(['lane']), 0)
                result = json.loads(stdout.getvalue())
                self.assertEqual(result['status'], 'rejected')
                self.assertEqual(result['error'], 'invalid_request')
                self.assertIn(name, result['fix'])
                if target:
                    self.assertIn(target, result['fix'])
                self.assertLess(len(result['fix']), 650)
                self.assertNotIn('private exception message', result['fix'])
                self.assertNotIn('\n', result['fix'])

    def test_project_placement_configuration_and_lane_verbs(self):
        import fabric_hosts as hosts
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'hosts.json'
            path.write_text(json.dumps({'schema_version': 1, 'local_host': 'laptop',
                'peers': {'workshop': {'ssh_destination': 'workshop'}},
                'projects': {'Repos/project': {'default_host': 'workshop',
                    'modes': {'read_only': 'laptop'}}}}))
            config = hosts.load_config(path)
            self.assertEqual(config.projects['Repos/project']['default_host'], 'workshop')
            verb, params = hosts.parse_request(json.dumps({'protocol_version': 1,
                'verb': 'lanes', 'params': {'project_path': 'Repos/project', 'input': {}}}).encode())
            self.assertEqual(verb, 'lanes')


    def test_federated_absolute_selector_resolves_host_run_dir_from_main_repository(self):
        import fabric_hosts as hosts
        import fabric_lanes

        with tempfile.TemporaryDirectory() as directory, patch.dict(os.environ, {
                'HOME': directory, 'AGENT_FABRIC_INSTANCE_ROOT': directory}):
            project = 'Repos/project'
            repository = Path(directory) / project
            repository.mkdir(parents=True)
            subprocess.run(['git', 'init', '-q', str(repository)], check=True)
            subprocess.run(['git', '-C', str(repository), 'config', 'user.name', 'Fixture'], check=True)
            subprocess.run(['git', '-C', str(repository), 'config', 'user.email', 'fixture@example.invalid'], check=True)
            subprocess.run(['git', '-C', str(repository), 'commit', '-q', '--allow-empty', '-m', 'fixture'], check=True)
            worktree = repository / '.worktrees/peer'
            subprocess.run(['git', '-C', str(repository), 'worktree', 'add', '-q', '--detach', str(worktree), 'HEAD'], check=True)
            run_dir = repository / '.agent-run/runs/shared-run'
            run_dir.mkdir(parents=True)
            owner = fabric_lanes.LaneFederation(hosts)
            row = {'id': 'shared-run-id', 'run_id': 'shared-run-id', 'task_id': 'batch-task', 'attempt': 1,
                'run_path': 'runs/shared-run', 'run_dir': str(run_dir)}
            with owner.database() as db:
                db.execute('INSERT INTO snapshots VALUES (?,?,?,?,?)',
                    (project, owner.config.local_host, 'lanes', time.time(), json.dumps([row])))
            selector = str(repository / '.agent-run/runs/../runs/shared-run')
            with patch.object(hosts, 'project_directory', return_value=worktree), \
                    patch.object(owner, 'worker', side_effect=hosts.HostError('selector_not_found', 'missing')):
                result = owner.reads('lanes', project, {'ids': [selector]})

            self.assertEqual(result['runs'][0]['run_dir'], str(run_dir))
            self.assertEqual(result['runs'][0]['host'], owner.config.local_host)

    def peer(self, request, **env):
        with tempfile.TemporaryDirectory() as directory:
            environment = {**os.environ, 'HOME': directory,
                           'AGENT_FABRIC_INSTANCE_ROOT': directory, **env}
            return subprocess.run([str(front_door_binary()), 'peer'],
                                  input=request, capture_output=True, env=environment)


    def test_invalid_requests_return_typed_errors(self):
        import fabric_hosts as hosts
        cases = [(b'{', 'bad_json'),
                 ('{"protocol_version":1,"verb":"hello"}'.encode('utf-16'), 'bad_json'),
                 (b'{} {}', 'bad_json'),
                 (b'x' * 65537, 'request_too_large'),
                 (b'{"protocol_version":1,"verb":"shell","params":{}}', 'unknown_verb'),
                 (b'{"protocol_version":1,"verb":"hello","params":{"command":"true"}}', 'invalid_request'),
                 (b'{"protocol_version":2,"verb":"doctor","params":{}}', 'protocol_mismatch'),
                 (b'{"protocol_version":1,"protocol_version":1,"verb":"hello"}', 'bad_json')]
        for raw, code in cases[:-1]:
            with self.subTest(code=code, raw=raw[:50]):
                with self.assertRaises(hosts.HostError) as caught:
                    hosts.parse_request(raw)
                self.assertEqual(caught.exception.code, code)

    def test_cursor_and_kiro_use_noninteractive_json_auth_status(self):
        import fabric_hosts as hosts
        with tempfile.TemporaryDirectory() as directory:
            binary = Path(directory) / 'provider'
            for adapter, arguments, output, code, expected in [
                ('cursor', ['status','--format','json'], {'isAuthenticated':True}, 0, 'usable'),
                ('cursor', ['status','--format','json'], {'isAuthenticated':False}, 0, 'unusable'),
                ('kiro', ['whoami','--format','json'], {'account':None}, 1, 'unusable'),
                ('kiro', ['whoami','--format','json'], {'account':{'status':'fixture'}}, 0, 'usable')]:
                with self.subTest(adapter=adapter, output=output):
                    binary.write_text('#!' + sys.executable + '\nimport sys,json\n' +
                        'assert sys.argv[1:] == ' + repr(arguments) + '\nprint(json.dumps(' + repr(output) + '))\nsys.exit(' + str(code) + ')\n')
                    binary.chmod(0o755)
                    self.assertEqual(hosts.signin_status(adapter, str(binary))['status'], expected)

    def test_keychain_failures_and_confinement_probe_timeouts_are_per_adapter_unknown(self):
        import fabric_hosts as hosts
        provider_exec = hosts._load_module(
            'provenant_provider_exec', ROOT / 'skills/orchestrate/scripts/provider_exec.py'
        )
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for binary in ['sandbox-exec', 'kiro-cli', 'claude']:
                path = root / binary
                path.write_text('#!' + sys.executable + '\nimport sys; print("keychain locked", file=sys.stderr); sys.exit(1)\n')
                path.chmod(0o755)
            def run(command, **kwargs):
                if str(command[0]).endswith('kiro-cli'):
                    raise subprocess.TimeoutExpired(command, 10)
                return subprocess.CompletedProcess(command, 0, stdout='', stderr='')
            environment = {**os.environ, 'PATH': str(root), 'PROVENANT_NO_OS_CONFINEMENT': '0'}
            try:
                with patch.dict(os.environ, environment, clear=True), patch.object(provider_exec.sys, 'platform', 'darwin'), patch.object(subprocess, 'run', side_effect=run):
                    provider_exec.provider_confinement._sandbox_exec_usable.cache_clear()
                    value = hosts.adapter_doctor('kiro', root)
                    self.assertEqual(value['confinement']['status'], 'unknown')
                    self.assertEqual(hosts.signin_status('claude', str(root / 'claude'))['status'], 'unknown')
            finally:
                provider_exec.provider_confinement._sandbox_exec_usable.cache_clear()


class ClientContracts(unittest.TestCase):
    def test_hangup_reaps_detached_ssh_before_returning(self):
        with git_isolated_tempdir() as directory:
            root = Path(directory)
            instance = root / 'instance/.agent-fabric'
            instance.mkdir(parents=True)
            (root / 'Repos/project').mkdir(parents=True)
            (instance / 'hosts.json').write_text(json.dumps({
                'schema_version': 1, 'local_host': 'laptop',
                'peers': {'workshop': {'ssh_destination': 'workshop', 'response_deadline': 5}}}))
            pid_file, shim = root / 'ssh.pid', root / 'ssh'
            shim.write_text('#!' + sys.executable + '\nimport os,time\n'
                            'open(' + repr(str(pid_file)) + ',"w").write(str(os.getpid()))\n'
                            'time.sleep(30)\n')
            shim.chmod(0o755)
            owner = subprocess.Popen([sys.executable, str(ROOT / 'scripts/fabric_hosts.py'),
                                      'hosts', 'doctor', '--json', 'workshop'],
                                     cwd=root / 'Repos/project', stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                     env={**os.environ, 'HOME': directory,
                                          'AGENT_FABRIC_INSTANCE_ROOT': str(instance.parent),
                                          'AGENT_FABRIC_SSH_PROGRAM': str(shim)})
            pid = None
            try:
                deadline = time.monotonic() + 3
                while not pid_file.exists() and time.monotonic() < deadline:
                    time.sleep(.02)
                self.assertTrue(pid_file.exists(), 'SSH fixture did not start')
                pid = int(pid_file.read_text())
                owner.send_signal(signal.SIGHUP)
                stdout, stderr = owner.communicate(timeout=5)
                with self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)
                self.assertEqual(json.loads(stdout)['error']['code'], 'hosts_cancelled', stderr)
            finally:
                if owner.poll() is None:
                    owner.kill()
                owner.communicate()
                if pid is not None:
                    try:
                        os.killpg(pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_overflowing_json_number_is_a_typed_peer_error(self):
        import fabric_hosts as hosts
        with tempfile.TemporaryDirectory() as directory:
            shim = Path(directory) / 'ssh'
            shim.write_text('#!' + sys.executable + '\n'
                            'print(\'{"protocol_version":1,"ok":true,"result":{"protocol_version":1,'
                            '"host":"workshop","revision":"fixture","extra":1e999}}\')\n')
            shim.chmod(0o755)
            response = hosts.PeerClient('workshop', hosts.PeerConfig('workshop'),
                                        ssh_program=str(shim)).call('hello')
            self.assertEqual(response.get('error', {}).get('code'), 'bad_response')

    def test_doctor_response_must_bind_the_requested_project_and_typed_identity(self):
        import fabric_hosts as hosts
        valid = {'protocol_version': 1, 'host': 'workshop', 'revision': 'fixture',
                 'project': {'path': 'Repos/project', 'present': True}, 'adapters': {},
                 'fabric_registrations': {}, 'python': {'version':'3.11.0','minimum':'3.11','supported':True},
                 'lane_temporary_paths': {'fits': False, 'max_path_bytes': 200, 'limit_bytes': 103}}
        with tempfile.TemporaryDirectory() as directory:
            shim = Path(directory) / 'ssh'
            for diagnostic in [{**valid, 'project': {'path': 'Repos/other', 'present': True}},
                               {**valid, 'protocol_version': True}, {**valid, 'revision': []}]:
                with self.subTest(diagnostic=diagnostic):
                    shim.write_text('#!' + sys.executable + '\nimport sys,json\nr=json.load(sys.stdin)\n'
                                    'result={"protocol_version":1,"host":"workshop","revision":"fixture"} '
                                    'if r["verb"]=="hello" else ' + repr(diagnostic) + '\n'
                                    'print(json.dumps({"protocol_version":1,"ok":True,"result":result}))\n')
                    shim.chmod(0o755)
                    response = hosts.PeerClient('workshop', hosts.PeerConfig('workshop'),
                                                ssh_program=str(shim)).call('doctor', {'project_path': 'Repos/project'})
                    self.assertEqual(response.get('error', {}).get('code'), 'bad_response')

    def test_transport_errors_and_protocol_write_decision(self):
        import fabric_hosts as hosts
        self.assertTrue(hasattr(hosts, 'PeerClient'), 'single peer client is missing')
        with tempfile.TemporaryDirectory() as directory:
            shim = Path(directory) / 'ssh'
            def client(program, deadline=1):
                shim.write_text('#!' + sys.executable + '\n' + program)
                shim.chmod(0o755)
                return hosts.PeerClient('workshop', hosts.PeerConfig('user@workshop', response_deadline=deadline), ssh_program=str(shim))
            unreachable = client('import sys; sys.exit(255)')
            self.assertEqual(unreachable.call('doctor')['error']['code'], 'unreachable')
            missing = client('import sys; print("provenant: command not found", file=sys.stderr); sys.exit(127)').call('doctor')
            self.assertEqual(missing['error']['code'], 'peer_command_not_found')
            self.assertIn('command not found', missing['error']['message'])
            failure = client('import sys; print("remote traceback", file=sys.stderr); sys.exit(1)').call('doctor')
            self.assertEqual(failure['error']['code'], 'peer_command_failed')
            self.assertIn('remote traceback', failure['error']['message'])
            noisy = client('import sys; sys.stderr.write("x" * 10000); sys.exit(1)').call('doctor')
            self.assertLessEqual(len(noisy['error']['message'].encode()), 4300)
            self.assertTrue(noisy['error']['message'].endswith('x' * 64))
            self.assertEqual(client('print("banner")').call('doctor')['error']['code'], 'bad_response')
            self.assertEqual(client('print("x" * 1048577)').call('doctor')['error']['code'], 'bad_response')
            started = time.monotonic()
            timeout = client('import time; time.sleep(10)', .1).call('doctor')
            self.assertEqual(timeout['error']['code'], 'timeout')
            self.assertEqual(timeout['reachability'], 'unreachable')
            self.assertLess(time.monotonic() - started, 2)
            self.assertEqual(client('print("{}")').call('doctor')['error']['code'], 'bad_response')
            program = "import sys,json; r=json.load(sys.stdin); print(json.dumps({'protocol_version':2,'ok':True,'result':{'protocol_version':2,'host':'workshop','revision':'fixture'}}))"
            mismatch = client(program)
            read = mismatch.call('hello')
            self.assertEqual(read['error']['code'], 'protocol_mismatch')
            self.assertEqual(read['reachability'], 'reachable')
            self.assertFalse(hosts.protocol_decision(2)['writes_allowed'])
            self.assertTrue(hosts.protocol_decision(1)['writes_allowed'])
            write = mismatch.call('dispatch', write=True)
            self.assertEqual(write['error']['code'], 'protocol_mismatch')
            self.assertNotIn('result', write)

    def test_old_protocol_v1_doctor_keeps_diagnostics_and_marks_python_unknown(self):
        import fabric_hosts as hosts
        diagnostic = {'protocol_version': 1, 'host': 'workshop', 'revision': 'previous',
            'project': {'path': 'Repos/project', 'present': True}, 'adapters': {},
            'fabric_registrations': {}, 'lane_temporary_paths':
            {'fits': True, 'max_path_bytes': 90, 'limit_bytes': 103}}
        with tempfile.TemporaryDirectory() as directory:
            shim = Path(directory) / 'ssh'
            shim.write_text('#!' + sys.executable + '\nimport sys,json\nr=json.load(sys.stdin)\n'
                + 'result={"protocol_version":1,"host":"workshop","revision":"previous"} '
                + 'if r["verb"]=="hello" else ' + repr(diagnostic) + '\n'
                + 'print(json.dumps({"protocol_version":1,"ok":True,"result":result}))\n')
            shim.chmod(0o755)
            response = hosts.PeerClient('workshop', hosts.PeerConfig('workshop'),
                ssh_program=str(shim)).call('doctor', {'project_path': 'Repos/project'})
            self.assertTrue(response['ok'], response)
            self.assertTrue(response['compatible'], response)
            self.assertEqual(response['result']['project'], diagnostic['project'])
            self.assertEqual(response['result']['lane_temporary_paths'], diagnostic['lane_temporary_paths'])
            runtime = response['result']['python']
            self.assertEqual(runtime['status'], 'unknown')
            self.assertIsNone(runtime['supported'])
            self.assertEqual(runtime['minimum'], '3.11')
            self.assertEqual(runtime['error']['code'], 'python_check_unavailable')
            self.assertIn('upgrade', runtime['error']['message'].lower())
            self.assertIn('3.11', runtime['error']['message'])

    def test_doctor_rejects_missing_or_malformed_diagnostic_fields_after_hello(self):
        import fabric_hosts as hosts
        valid = {'protocol_version':1, 'host':'workshop', 'revision':'fixture',
                 'project':{'path':'Repos/project','present':True}, 'adapters':{},
                 'python':{'version':'3.11.0','minimum':'3.11','supported':True}, 'fabric_registrations':{}, 'lane_temporary_paths':{'fits':False,'max_path_bytes':120,'limit_bytes':103}}
        malformed = [{}, {**valid, 'adapters':{'codex':{'executable':'codex','signin':{'status':'probably'},'confinement':{'status':'effective'}}}},
                     {**valid, 'fabric_registrations':{'codex':{'registered':'yes'}}},
                     {**valid, 'adapters':{'codex':{'executable':None,'signin':{'status':[]},'confinement':{'status':'effective'}}}},
                     {**valid, 'lane_temporary_paths':{'fits':True,'limit_bytes':'103'}},
                     {**valid, 'python':{'version':'3.10.0','minimum':'3.11','supported':True}},
                     {**valid, 'python':{'version':'3.10.0','minimum':'3.11','supported':False}},
                     {**valid, 'python':None}]
        with tempfile.TemporaryDirectory() as directory:
            shim = Path(directory) / 'ssh'
            for diagnostic in malformed:
                with self.subTest(diagnostic=diagnostic):
                    shim.write_text('#!' + sys.executable + '\n' +
                        'import sys,json; r=json.load(sys.stdin); result={"protocol_version":1,"host":"workshop","revision":"fixture"} if r["verb"]=="hello" else ' + repr(diagnostic) + '; print(json.dumps({"protocol_version":1,"ok":True,"result":result}))')
                    shim.chmod(0o755)
                    response = hosts.PeerClient('workshop', hosts.PeerConfig('workshop'), ssh_program=str(shim)).call('doctor')
                    self.assertEqual(response['error']['code'], 'bad_response')

    def test_openssh_arguments_and_first_contact_hello(self):
        import fabric_hosts as hosts
        self.assertTrue(hasattr(hosts, 'PeerClient'), 'single peer client is missing')
        with tempfile.TemporaryDirectory() as directory:
            shim, log = Path(directory) / 'ssh', Path(directory) / 'calls'
            shim.write_text('#!' + sys.executable + '\n' +
                'import sys,json\nr=json.load(sys.stdin)\n' +
                'with open(' + repr(str(log)) + ',"a") as f: f.write(json.dumps([sys.argv[1:],r])+"\\n")\n' +
                'print(json.dumps({"protocol_version":1,"ok":True,"result":{"protocol_version":1,"host":"workshop","revision":"fixture"}}))\n')
            shim.chmod(0o755)
            peer = hosts.PeerClient('workshop', hosts.PeerConfig('workshop'), ssh_program=str(shim))
            self.assertEqual(peer.call('hello')['reachability'], 'reachable')
            self.assertEqual(peer.call('hello')['reachability'], 'reachable')
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0][0], ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5', '--', 'workshop', '.local/bin/provenant peer'])
            self.assertEqual(calls[0][1]['verb'], 'hello')

    def test_ssh_end_of_options_keeps_destination_and_remote_command_out_of_option_parsing(self):
        import fabric_hosts as hosts
        with tempfile.TemporaryDirectory() as directory:
            shim, log = Path(directory) / 'ssh', Path(directory) / 'argv.json'
            shim.write_text('#!' + sys.executable + '\nimport json,sys; open(' + repr(str(log)) + ',"w").write(json.dumps(sys.argv[1:]))\n')
            shim.chmod(0o755)
            client = hosts.PeerClient('workshop', hosts.PeerConfig('workshop'), ssh_program=str(shim))
            client.call('hello')
            args = json.loads(log.read_text())
            self.assertEqual(args[4], '--')
            self.assertEqual(args[5:], ['workshop', '.local/bin/provenant peer'])
            parsed = subprocess.run(['ssh', '-F', os.devnull, '-G', *args], capture_output=True, text=True)
            self.assertEqual(parsed.returncode, 0, parsed.stderr)
            self.assertIn('hostname workshop', parsed.stdout.lower())

    def test_peer_command_and_destination_cannot_start_with_an_ssh_option(self):
        import fabric_hosts as hosts
        configs = [
            {'schema_version': 1, 'local_host': 'laptop', 'peers': {'workshop': {'ssh_destination': '-oProxyCommand=touch'}}},
            {'schema_version': 1, 'local_host': 'laptop', 'peers': {'workshop': {'ssh_destination': 'workshop', 'peer_command': '-oProxyCommand peer'}}},
        ]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'hosts.json'
            for config in configs:
                with self.subTest(config=config):
                    path.write_text(json.dumps(config))
                    with self.assertRaises(hosts.HostError) as error:
                        hosts.load_config(path)
                    self.assertEqual(error.exception.code, 'invalid_config')
        with tempfile.TemporaryDirectory() as directory:
            invoked = Path(directory) / 'invoked'
            shim = Path(directory) / 'ssh'
            shim.write_text('#!' + sys.executable + '\nopen(' + repr(str(invoked)) + ',"w").write("called")\n')
            shim.chmod(0o755)
            for peer in [hosts.PeerConfig('-oProxyCommand=touch marker'),
                         hosts.PeerConfig('workshop', '-oProxyCommand peer')]:
                response = hosts.PeerClient('workshop', peer, ssh_program=str(shim)).call('hello')
                self.assertEqual(response['error']['code'], 'invalid_config')
            self.assertFalse(invoked.exists())

    def test_deep_peer_json_is_bad_response_and_does_not_abort_healthy_host(self):
        import fabric_hosts as hosts
        with tempfile.TemporaryDirectory() as directory:
            shim = Path(directory) / 'ssh'
            shim.write_text(
                '#!' + sys.executable + '\n'
                'import json,sys\nrequest=json.load(sys.stdin)\n'
                'if sys.argv[-2] == "broken":\n'
                ' sys.stdout.write(\'{"protocol_version":1,"ok":true,"result":\' + "[" * 10000 + "0" + "]" * 10000 + "}\\n")\n'
                'else:\n'
                ' result={"protocol_version":1,"host":"healthy","revision":None} if request["verb"]=="hello" else '
                '{"protocol_version":1,"host":"healthy","revision":None,"project":{"path":"Repos/project","present":True},"adapters":{},"fabric_registrations":{},"python":{"version":"3.11.0","minimum":"3.11","supported":true},"lane_temporary_paths":{"fits":True,"max_path_bytes":90,"limit_bytes":103}}\n'
                ' print(json.dumps({"protocol_version":1,"ok":True,"result":result}))\n')
            shim.chmod(0o755)
            config = hosts.HostsConfig('laptop', {'broken': hosts.PeerConfig('broken'),
                                                   'healthy': hosts.PeerConfig('healthy')})
            with patch.dict(os.environ, {'AGENT_FABRIC_SSH_PROGRAM': str(shim)}), \
                 patch.object(hosts, 'project_identity', return_value='Repos/project'):
                value = hosts.hosts_doctor(config, ['broken', 'healthy'])
            rows = {row['host']: row for row in value['hosts']}
            self.assertEqual(rows['broken']['error']['code'], 'bad_response')
            self.assertEqual(rows['healthy']['reachability'], 'reachable')

    def test_local_doctor_error_is_a_row_and_peer_rows_survive(self):
        import fabric_hosts as hosts
        config = hosts.HostsConfig('laptop', {'workshop': hosts.PeerConfig('workshop')})
        peer_row = {'host': 'workshop', 'reachability': 'reachable', 'ok': True, 'result': {'fixture': True}}
        with patch.object(hosts, 'project_identity', return_value='Repos/project'), \
             patch.object(hosts, 'doctor', side_effect=hosts.HostError('doctor_unavailable', 'catalogue unavailable')), \
             patch.object(hosts.PeerClient, 'call', return_value=peer_row):
            value = hosts.hosts_doctor(config, ['laptop', 'workshop'])
        rows = {row['host']: row for row in value['hosts']}
        self.assertEqual(rows['laptop']['error']['code'], 'doctor_unavailable')
        self.assertEqual(rows['laptop']['reachability'], 'reachable')
        self.assertEqual(rows['workshop'], peer_row)
        with patch.object(hosts, 'project_identity', return_value='Repos/project'), \
             patch.object(hosts, 'doctor', side_effect=OSError('unreadable catalogue')), \
             patch.object(hosts.PeerClient, 'call', return_value=peer_row):
            value = hosts.hosts_doctor(config, ['laptop', 'workshop'])
        rows = {row['host']: row for row in value['hosts']}
        self.assertEqual(rows['laptop']['error']['code'], 'doctor_unavailable')
        self.assertEqual(rows['workshop'], peer_row)

    def test_product_import_paths_do_not_depend_on_cwd_or_argv0(self):
        script = (
            'import importlib.util, sys; '
            'original_path = list(sys.path); '
            f'f=importlib.util.spec_from_file_location("fabric_hosts", {str(ROOT / "scripts/fabric_hosts.py")!r}); '
            'm=importlib.util.module_from_spec(f); sys.modules["fabric_hosts"]=m; f.loader.exec_module(m); '
            'assert sys.path == original_path; '
            'm._load_module("provenant_model_route", m.PRODUCT_ROOT / "scripts/model_route.py"); '
            'm._load_module("adapters", m.PRODUCT_ROOT / "skills/orchestrate/scripts/adapters/__init__.py", package=True); '
            'assert sys.path == original_path'
        )
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([sys.executable, '-c', script], cwd=directory,
                                    capture_output=True, text=True, env={**os.environ, 'PYTHONPATH': ''})
        self.assertEqual(result.returncode, 0, result.stderr)


class IdentifierContracts(unittest.TestCase):
    def test_parse_format_and_cross_host_ambiguity(self):
        import fabric_hosts as hosts
        self.assertTrue(hasattr(hosts, 'parse_identifier'), 'qualified identifier behaviour is missing')
        self.assertEqual(hosts.parse_identifier('mcp-a', 'laptop'), ('mcp-a', 'laptop'))
        self.assertEqual(hosts.parse_identifier('mcp-a@workshop', 'laptop'), ('mcp-a', 'workshop'))
        self.assertEqual(hosts.format_identifier('mcp-a', 'workshop'), 'mcp-a@workshop')
        for value in ['a@@workshop', '@workshop', 'a@Workshop', 'a@', 'a b']:
            with self.assertRaises(hosts.HostError) as error:
                hosts.parse_identifier(value, 'laptop')
            self.assertEqual(error.exception.code, 'invalid_identifier')
        records = [{'id': 'mcp-a', 'host': 'laptop'}, {'id': 'mcp-a', 'host': 'workshop'}]
        with self.assertRaises(hosts.HostError) as error:
            hosts.resolve_selector('mcp-a', records, 'laptop')
        self.assertEqual(error.exception.code, 'ambiguous_selector')
        self.assertEqual(hosts.resolve_selector('mcp-a@workshop', records, 'laptop'), records[1])
        self.assertEqual(hosts.resolve_selector('mcp-a', records[:1], 'laptop'), records[0])


class ConfigContracts(unittest.TestCase):
    def command(self, config):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            if config is not None:
                (root / '.agent-fabric').mkdir()
                (root / '.agent-fabric/hosts.json').write_text(json.dumps(config))
            return subprocess.run([str(front_door_binary()), 'hosts', 'list', '--json'],
                                  capture_output=True, text=True,
                                  env={**os.environ, 'AGENT_FABRIC_INSTANCE_ROOT': directory})


    def test_instance_root_must_be_absolute(self):
        result = subprocess.run([str(ROOT / 'scripts/fabric-hosts'), 'hosts', 'list', '--json'],
            capture_output=True, text=True, env={**os.environ, 'AGENT_FABRIC_INSTANCE_ROOT': '.'})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout)['error']['code'], 'invalid_config')


class HostDiagnosticsContracts(unittest.TestCase):

    def test_temporary_directory_estimate_uses_filesystem_path_limits(self):
        import fabric_hosts as hosts
        with tempfile.TemporaryDirectory() as directory:
            result = hosts.lane_temporary_paths(Path(directory) / 'project')
            self.assertTrue(result['fits'], result)
            self.assertEqual(result['constraint'], 'filesystem_path')
            self.assertGreater(result['limit_bytes'], 103)
            with patch.object(os, 'pathconf', side_effect=lambda path, name: 128 if name == 'PC_PATH_MAX' else 255):
                self.assertFalse(hosts.lane_temporary_paths(Path(directory) / 'project')['fits'])
            with patch.object(os, 'pathconf', side_effect=lambda path, name: 4096 if name == 'PC_PATH_MAX' else 16):
                self.assertFalse(hosts.lane_temporary_paths(Path(directory) / 'project')['fits'])

    def test_browser_socket_budget_reserves_suffix_and_counts_encoded_bytes(self):
        import fabric_hosts as hosts
        suffix = '/.org.chromium.Chromium.XXXXXX/SingletonSocket'
        for platform, limit in [('darwin', 104), ('linux', 108)]:
            with self.subTest(platform=platform), patch.object(hosts.sys, 'platform', platform):
                temporary = Path('/tmp/' + 'x' * (limit - 1 - len(suffix) - len('/tmp/')))
                self.assertTrue(hosts.browser_socket_paths(temporary)['fits'])
                result = hosts.browser_socket_paths(Path(str(temporary) + 'x'))
                self.assertFalse(result['fits'])
                self.assertEqual(result['max_path_bytes'], limit)
                # One non-ASCII character needs two UTF-8 bytes.
                self.assertFalse(hosts.browser_socket_paths(Path(str(temporary)[:-1] + 'é'))['fits'])

    def test_browser_temporary_directory_stays_short_for_long_attempt_paths(self):
        import fabric_hosts as hosts
        attempt = Path('/project') / ('long-name-' * 20) / '.agent-run/runs/run/dispatch/tasks/task/attempt-001'
        temporary = hosts.browser_temporary_directory(attempt)
        self.assertEqual(temporary.parent, Path('/tmp').resolve())
        self.assertRegex(temporary.name, r'^fb-[0-9a-f]{12}$')
        self.assertEqual(temporary, hosts.browser_temporary_directory(attempt))
        self.assertNotEqual(temporary, hosts.browser_temporary_directory(attempt.with_name('attempt-002')))
        with patch.object(hosts.sys, 'platform', 'darwin'):
            self.assertTrue(hosts.browser_socket_paths(temporary)['fits'])

    def test_browser_temporary_directory_is_private_and_refuses_unsafe_existing_paths(self):
        import fabric_hosts as hosts
        with tempfile.TemporaryDirectory() as directory:
            attempt = Path(directory) / 'attempt'
            temporary = hosts.browser_temporary_directory(attempt)
            try:
                self.assertEqual(hosts.browser_temporary_directory(attempt, create=True), temporary)
                self.assertEqual(temporary.stat().st_uid, os.getuid())
                self.assertEqual(temporary.stat().st_mode & 0o777, 0o700)
                temporary.chmod(0o755)
                hosts.browser_temporary_directory(attempt, create=True)
                self.assertEqual(temporary.stat().st_mode & 0o777, 0o700)
                from types import SimpleNamespace
                foreign = SimpleNamespace(st_mode=temporary.stat().st_mode, st_uid=os.getuid() + 1)
                with patch.object(hosts.os, 'fstat', return_value=foreign):
                    with self.assertRaises(PermissionError):
                        hosts.browser_temporary_directory(attempt, create=True)
                temporary.rmdir()
                temporary.symlink_to(directory, target_is_directory=True)
                with self.assertRaises(OSError):
                    hosts.browser_temporary_directory(attempt, create=True)
                self.assertTrue(temporary.is_symlink())
                temporary.unlink()
                temporary.write_text('not a directory')
                with self.assertRaises(OSError):
                    hosts.browser_temporary_directory(attempt, create=True)
            finally:
                if temporary.is_symlink() or temporary.is_file():
                    temporary.unlink()
                elif temporary.exists():
                    temporary.rmdir()

    def test_doctor_interpreter_probe_failure_names_python_minimum(self):
        with tempfile.TemporaryDirectory() as directory:
            interpreter = Path(directory) / 'python'
            interpreter.write_text('#!/bin/sh\nexit 1\n')
            interpreter.chmod(0o755)
            result = subprocess.run([str(ROOT / 'scripts/fabric-hosts'), 'hosts', 'doctor', '--json'],
                                    capture_output=True, text=True,
                                    env={**os.environ, 'HARNESS_PYTHON': str(interpreter)})
            self.assertEqual(result.returncode, 3, result.stdout + result.stderr)
            self.assertIn('Python 3.11 or newer', result.stderr)

    def test_doctor_reports_python_minimum_and_clear_failure(self):
        import fabric_hosts as hosts
        with tempfile.TemporaryDirectory() as directory:
            environment = {'HOME': directory, 'AGENT_FABRIC_INSTANCE_ROOT': directory}
            with patch.dict(os.environ, environment):
                for version, supported in [((3, 10, 14), False), ((3, 11, 0), True)]:
                    with self.subTest(version=version), patch.object(sys, 'version_info', version):
                        result = hosts.doctor('project')
                        self.assertEqual(result['python']['version'], '.'.join(map(str, version)))
                        self.assertEqual(result['python']['minimum'], '3.11')
                        self.assertEqual(result['python']['supported'], supported)
                        if not supported:
                            self.assertEqual(result['python']['error']['code'], 'python_too_old')
                            self.assertIn('3.11', result['python']['error']['message'])


class FabricGitDiagnosticContracts(unittest.TestCase):
    def git_failure(self, directory, **fields):
        from types import SimpleNamespace
        import fabric_git
        import fabric_hosts as hosts
        code = fabric_git.CodeTransport(SimpleNamespace(h=hosts))
        result = SimpleNamespace(**{'returncode': 0, 'timed_out': False, 'stdout': '', 'stderr': '',
            'stdout_truncated': False, 'stderr_truncated': False, **fields})
        load = code.h._load_module
        def load_module(name, path):
            if name == 'provenant_bounded_process':
                return SimpleNamespace(run_bounded=lambda *args, **kwargs: result)
            return load(name, path)
        with patch.object(code.h, '_load_module', side_effect=load_module):
            with self.assertRaises(hosts.HostError) as caught:
                code.git(Path(directory), 'rev-parse', 'HEAD')
        self.assertEqual(caught.exception.code, 'code_transport_failed')
        return str(caught.exception)

    def test_git_failure_reports_exit_code_and_repository(self):
        with tempfile.TemporaryDirectory() as directory:
            message = self.git_failure(directory, returncode=128)
            self.assertIn(f'failed in {directory}: exit 128', message)
            self.assertNotIn('no error output', message)

    def test_git_failure_reports_truncated_output(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertIn('stdout truncated', self.git_failure(directory, stdout='', stderr='detail', stdout_truncated=True))

    def test_git_failure_reports_signal_truncation_and_last_error_line(self):
        with tempfile.TemporaryDirectory() as directory:
            message = self.git_failure(directory, returncode=-9, stderr='warning\nfatal: x', stderr_truncated=True)
            self.assertTrue(message.endswith('exit -9 (SIGKILL); stderr truncated: fatal: x'), message)
            long_line = self.git_failure(directory, returncode=1, stderr='fatal: ' + 'y' * 300)
            self.assertTrue(long_line.endswith('y…'), long_line)


class TransportFilesystemContracts(unittest.TestCase):
    def test_project_root_accepts_case_different_git_toplevel_for_same_directory(self):
        import fabric_hosts as hosts
        import fabric_lanes
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            repo = home / 'Repos/project'
            repo.mkdir(parents=True)
            subprocess.run(['git', 'init', '-q', str(repo)], check=True)
            alias = repo.with_name('PROJECT')
            if not alias.exists():
                alias.symlink_to(repo, target_is_directory=True)
            binary = home / 'bin'
            binary.mkdir()
            executable = binary / 'git'
            import shutil
            actual_git = shutil.which('git')
            executable.write_text('#!' + sys.executable + '\nimport os,sys\n'
                + 'if "--show-toplevel" in sys.argv: print(' + repr(str(alias)) + '); sys.exit(0)\n'
                + 'os.execv(' + repr(actual_git) + ', ["git", *sys.argv[1:]])\n')
            executable.chmod(0o755)
            resolve = Path.resolve
            def keep_git_spelling(path, *args, **kwargs):
                # Emulate a case-insensitive filesystem preserving Git's spelling.
                return path if path == alias else resolve(path, *args, **kwargs)
            environment = {'HOME': str(home), 'PATH': str(binary) + os.pathsep + os.environ['PATH'],
                'AGENT_FABRIC_INSTANCE_ROOT': str(home / '.agents')}
            real_popen = subprocess.Popen
            def popen(command, *args, **kwargs):
                if command[0] == 'git':
                    command = [str(executable), *command[1:]]
                return real_popen(command, *args, **kwargs)
            with patch.dict(os.environ, environment), patch.object(Path, 'resolve', keep_git_spelling), \
                    patch.object(subprocess, 'Popen', side_effect=popen):
                federation = fabric_lanes.LaneFederation(hosts)
                federation.config = hosts.HostsConfig(projects={'Repos/project': {}})
                code = federation.code()
                self.assertEqual(code.repo('Repos/project'), repo)
                self.assertEqual(code.repo('Repos/project', receiving=True), repo)
                self.assertEqual(hosts.project_identity(repo), 'Repos/project')

    def test_linked_root_accepts_case_alias_for_its_git_backlink(self):
        import fabric_hosts as hosts
        helper = hosts.lane_owner().code().w
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory) / 'project'
            repo.mkdir()
            def git(*args):
                subprocess.run(['git', '-C', str(repo), *args], check=True, capture_output=True,
                    env={**os.environ, 'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                         'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'})
            git('init', '-q')
            git('commit', '--allow-empty', '-qm', 'base')
            linked = repo / '.worktrees/review'
            git('worktree', 'add', '--detach', str(linked), 'HEAD')
            alias = linked.with_name('REVIEW')
            if not alias.exists():
                alias.symlink_to(linked, target_is_directory=True)
            resolve = Path.resolve
            def case_spelling(path, *args, **kwargs):
                return path if path in {alias, alias / '.git'} else resolve(path, *args, **kwargs)
            real_git = helper.git
            def case_git(path, *args, **kwargs):
                result = real_git(path, *args, **kwargs)
                if args == ('rev-parse', '--show-toplevel'):
                    result.stdout = str(alias) + '\n'
                return result
            with patch.object(Path, 'resolve', case_spelling), patch.object(helper, 'git', side_effect=case_git):
                self.assertTrue(helper.same_directory(helper.owning_root(linked), linked))

    def test_git_directory_sync_accepts_unsupported_filesystems_only(self):
        import errno
        import fabric_git
        with tempfile.TemporaryDirectory() as directory:
            for failure in (errno.EINVAL, errno.ENOTSUP):
                with self.subTest(errno=failure), patch.object(os, 'fsync', side_effect=OSError(failure, 'unsupported')):
                    fabric_git.CodeTransport.sync_directory(Path(directory))
            with patch.object(os, 'fsync', side_effect=OSError(errno.EIO, 'I/O error')):
                with self.assertRaises(OSError):
                    fabric_git.CodeTransport.sync_directory(Path(directory))

    def test_custody_write_and_unlink_accept_unsupported_directory_sync(self):
        import errno
        import stat
        import fabric_hosts as hosts
        custody = hosts._load_module('provenant_fabric_custody', ROOT / 'skills/_shared/custody.py')
        real_fsync = os.fsync
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for failure in (errno.EINVAL, errno.ENOTSUP):
                def fsync(descriptor):
                    if stat.S_ISDIR(os.fstat(descriptor).st_mode):
                        raise OSError(failure, 'unsupported')
                    real_fsync(descriptor)
                with self.subTest(errno=failure), patch.object(os, 'fsync', side_effect=fsync):
                    custody.atomic_write_contained(root, 'input.json', b'{}')
                    self.assertEqual((root / 'input.json').read_bytes(), b'{}')
                    custody.unlink_contained_regular(root, 'input.json')
                    self.assertFalse((root / 'input.json').exists())
            with patch.object(os, 'fsync', side_effect=OSError(errno.EINVAL, 'file sync failed')):
                with self.assertRaises(custody.OwnedFileError):
                    custody.atomic_write_contained(root, 'input.json', b'{}')


@unittest.skipUnless(os.environ.get('PROVENANT_SSHD_TESTS') == '1' and Path('/usr/sbin/sshd').is_file(),
                     'requires PROVENANT_SSHD_TESTS=1 and /usr/sbin/sshd')
class SshdAcceptance(unittest.TestCase):
    def test_forced_authorized_key_ignores_a_malicious_requested_command(self):
        import getpass
        import shutil
        import time

        with git_isolated_tempdir() as directory:
            root = Path(directory)
            local_home, remote_home = root / 'local-home', root / 'remote-home'
            workspace = local_home / 'Repos/project'
            workspace.mkdir(parents=True)
            (remote_home / '.local/bin').mkdir(parents=True)
            (remote_home / '.agents/.agent-fabric').mkdir(parents=True)
            (local_home / '.agents/.agent-fabric').mkdir(parents=True)
            (remote_home / 'Repos/project').mkdir(parents=True)
            (remote_home / '.agents/.agent-fabric/hosts.json').write_text(
                json.dumps({'schema_version': 1, 'local_host': 'workshop'}))
            shutil.copy2(front_door_binary(), remote_home / '.local/bin/provenant')
            (remote_home / '.agents/.agent-fabric/product-root.json').write_text(
                json.dumps({'schema_version': 1, 'product_root': str(ROOT)}))

            host_key, client_key = root / 'host_key', root / 'client_key'
            ssh_binary = shutil.which('ssh')
            self.assertIsNotNone(ssh_binary, 'OpenSSH client is required with sshd')
            for key in (host_key, client_key):
                generated = subprocess.run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-f', str(key)],
                                           capture_output=True, text=True)
                self.assertEqual(generated.returncode, 0, generated.stderr)
            requested_marker = root / 'malicious-command-ran'
            forced = ('env HOME=' + shlex.quote(str(remote_home)) + ' AGENT_FABRIC_INSTANCE_ROOT='
                      + shlex.quote(str(remote_home / '.agents')) + ' '
                      + shlex.quote(str(remote_home / '.local/bin/provenant')) + ' peer')
            forced_option = forced.replace('\\', '\\\\').replace('"', '\\"')
            authorized = root / 'authorized_keys'
            authorized.write_text('command="' + forced_option + '",restrict '
                                  + (root / 'client_key.pub').read_text().strip() + '\n')

            with socket.socket() as listener:
                listener.bind(('127.0.0.1', 0))
                port = listener.getsockname()[1]
            ssh_config = root / 'ssh_config'
            ssh_config.write_text(
                'Host fabric-acceptance\n HostName 127.0.0.1\n Port ' + str(port) + '\n User ' + getpass.getuser()
                + '\n IdentityFile ' + str(client_key) + '\n IdentitiesOnly yes\n BatchMode yes'
                + '\n StrictHostKeyChecking no\n UserKnownHostsFile /dev/null\n LogLevel ERROR\n')
            server_config = root / 'sshd_config'
            server_config.write_text(
                'Port ' + str(port) + '\nListenAddress 127.0.0.1\nHostKey ' + str(host_key)
                + '\nPidFile ' + str(root / 'sshd.pid') + '\nAuthorizedKeysFile ' + str(authorized)
                + '\nStrictModes no\nPubkeyAuthentication yes\nPasswordAuthentication no'
                + '\nKbdInteractiveAuthentication no\nPermitRootLogin prohibit-password\nUsePAM no\nLogLevel ERROR\n')
            checked = subprocess.run(['/usr/sbin/sshd', '-t', '-f', str(server_config)],
                                     capture_output=True, text=True)
            self.assertEqual(checked.returncode, 0, checked.stderr)
            server_log_path = root / 'sshd.log'
            server_log = server_log_path.open('wb')
            server = subprocess.Popen(['/usr/sbin/sshd', '-D', '-e', '-f', str(server_config)],
                                      stdout=subprocess.DEVNULL, stderr=server_log, start_new_session=True)
            try:
                shim = root / 'ssh-override'
                shim.write_text('#!' + sys.executable + '\nimport os,sys\nos.execv(' + repr(ssh_binary) + ', ["ssh", "-F", '
                                + repr(str(ssh_config)) + ', "fabric-acceptance", '
                                + repr('touch ' + str(requested_marker)) + '])\n')
                shim.chmod(0o755)
                local_config = {'schema_version': 1, 'local_host': 'laptop',
                                'peers': {'workshop': {'ssh_destination': 'fabric-acceptance'}}}
                (local_home / '.agents/.agent-fabric/hosts.json').write_text(json.dumps(local_config))
                environment = {**os.environ, 'HOME': str(local_home),
                               'AGENT_FABRIC_INSTANCE_ROOT': str(local_home / '.agents'),
                               'AGENT_FABRIC_SSH_PROGRAM': str(shim), 'GIT_CEILING_DIRECTORIES': str(root)}
                for _ in range(50):
                    if server.poll() is not None:
                        self.fail('sshd exited: ' + server_log_path.read_text(errors='replace'))
                    try:
                        with socket.create_connection(('127.0.0.1', port), timeout=.2):
                            break
                    except OSError:
                        pass
                    time.sleep(.05)
                result = subprocess.run([str(front_door_binary()), 'hosts', 'doctor', '--json'],
                                        cwd=workspace, capture_output=True, text=True, env=environment, timeout=20)
                self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
                rows = {row['host']: row for row in json.loads(result.stdout)['hosts']}
                self.assertEqual(rows['workshop']['reachability'], 'reachable',
                                 (rows['workshop'], server_log_path.read_text(errors='replace')))
                self.assertTrue(rows['workshop']['ok'], rows['workshop'])
                self.assertEqual(rows['workshop']['result']['project'], {'path': 'Repos/project', 'present': True})
                self.assertFalse(requested_marker.exists())
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)
                server_log.close()


if __name__ == '__main__':
    unittest.main()


def test_nested_status_read_bypass_preserves_local_only_writes(monkeypatch):
    from types import SimpleNamespace
    import fabric_lanes
    owner = fabric_lanes.LaneFederation.__new__(fabric_lanes.LaneFederation)
    owner.config = SimpleNamespace(peers={'peer': {}}, local_host='here')
    owner.config_error = None
    owner.request_project = lambda *_args: ('Repos/project', None)
    owner.reads = lambda *_args: {'runs': [{'run_id': 'mcp-child@peer', 'status': 'ok'}]}
    monkeypatch.setenv('PROVENANT_HOST_LOCAL_ONLY', '1')
    assert owner.client_call('status', '.', {}) == {'local': True}
    assert owner.client_call('status', '.', {}, federated_read=True)['runs'][0]['status'] == 'ok'
    assert owner.client_call('dispatch', '.', {}, federated_read=True) == {'local': True}


def test_retired_peer_lane_owner_returns_typed_unavailability(tmp_path):
    import fabric_hosts as hosts
    import fabric_lanes
    owner = fabric_lanes.LaneFederation(hosts)
    with unittest.TestCase().assertRaises(hosts.HostError) as caught:
        owner.worker('dispatch', 'Repos/project', {'prompt': 'hello'})
    assert caught.exception.code == 'lane_owner_unavailable'
