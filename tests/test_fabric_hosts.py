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
from contextlib import contextmanager
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
REPOSITORY_ROOT = ROOT.parent.parent
sys.path.insert(0, str(ROOT / 'scripts'))


@contextmanager
def git_isolated_tempdir():
    """Avoid inheriting any enclosing project's Git identity from $TMPDIR."""
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory).resolve()
        enclosing = subprocess.run(['git', '-C', str(path), 'rev-parse', '--show-toplevel'],
                                   capture_output=True, env={key: value for key, value in os.environ.items()
                                                            if not key.startswith('GIT_')})
        if enclosing.returncode != 0:
            yield directory
            return
    scratch = next((candidate for candidate in (Path('/private/tmp'), Path('/var/tmp'))
                    if candidate.is_dir() and os.access(candidate, os.W_OK)), None)
    if scratch is None:
        raise RuntimeError('$TMPDIR is inside the checkout and no external scratch directory is writable')
    with tempfile.TemporaryDirectory(dir=scratch) as directory:
        yield directory


class PeerContracts(unittest.TestCase):
    def peer(self, request, **env):
        with tempfile.TemporaryDirectory() as directory:
            environment = {**os.environ, 'HOME': directory,
                           'AGENT_FABRIC_INSTANCE_ROOT': directory, **env}
            return subprocess.run([str(ROOT / 'scripts/provenant'), 'peer'],
                                  input=request, capture_output=True, env=environment)

    def test_peer_doctor_starts_with_a_nonlogin_system_path(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([str(ROOT / 'scripts/provenant'), 'peer'],
                input=json.dumps({'protocol_version':1,'verb':'doctor','params':{'project_path':'Repos/project'}}),
                capture_output=True, text=True, env={**os.environ,'HOME':directory,
                    'AGENT_FABRIC_INSTANCE_ROOT':directory, 'PATH':'/usr/bin:/bin'})
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            self.assertTrue(json.loads(result.stdout)['ok'])

    def test_invalid_requests_return_typed_errors(self):
        cases = [(b'{', 'bad_json'),
                 ('{"protocol_version":1,"verb":"hello"}'.encode('utf-16'), 'bad_json'),
                 (b'{} {}', 'bad_json'),
                 (b'x' * 65537, 'request_too_large'),
                 (b'{"protocol_version":1,"verb":"shell","params":{}}', 'unknown_verb'),
                 (b'{"protocol_version":1,"verb":"hello","params":{"command":"true"}}', 'invalid_request'),
                 (b'{"protocol_version":2,"verb":"doctor","params":{}}', 'protocol_mismatch'),
                 (b'{"protocol_version":1,"protocol_version":1,"verb":"hello"}', 'bad_json')]
        for raw, code in cases:
            with self.subTest(code=code, raw=raw[:50]):
                response = self.peer(raw)
                self.assertNotEqual(response.returncode, 0)
                value = json.loads(response.stdout)
                self.assertFalse(value['ok'])
                self.assertEqual(value['error']['code'], code)
                self.assertEqual(response.stderr, b'')

    def test_hello_is_one_versioned_response_even_under_a_forced_command(self):
        response = self.peer(json.dumps({'protocol_version': 1, 'verb': 'hello', 'params': {}}).encode(),
                             SSH_ORIGINAL_COMMAND='touch /tmp/peer-injection')
        self.assertEqual(response.returncode, 0, response.stderr)
        value = json.loads(response.stdout)
        self.assertTrue(value['ok'])
        self.assertEqual(value['protocol_version'], 1)
        self.assertEqual(value['result']['host'], 'local')
        self.assertIn('revision', value['result'])
        self.assertEqual(response.stderr, b'')


class DoctorContracts(unittest.TestCase):
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
        sys.path.insert(0, str(ROOT / 'skills/orchestrate/scripts'))
        import provider_exec
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
            with patch.dict(os.environ, environment, clear=True), patch.object(provider_exec.sys, 'platform', 'darwin'), patch.object(subprocess, 'run', side_effect=run):
                provider_exec._sandbox_exec_usable.cache_clear()
                value = hosts.adapter_doctor('kiro', root)
                self.assertEqual(value['confinement']['status'], 'unknown')
                self.assertEqual(hosts.signin_status('claude', str(root / 'claude'))['status'], 'unknown')
            provider_exec._sandbox_exec_usable.cache_clear()

    def test_peer_doctor_reports_host_local_checks_and_bounds_project_paths(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'Repos/project').mkdir(parents=True)
            instance = root / 'instance/.agent-fabric'
            instance.mkdir(parents=True)
            (instance / 'hosts.json').write_text(json.dumps({'schema_version': 1, 'local_host': 'workshop'}))
            bin_dir = root / 'bin'
            bin_dir.mkdir()
            claude = bin_dir / 'claude'
            claude.write_text('#!' + sys.executable + '\nimport json; print(json.dumps({"loggedIn":False}))\n')
            claude.chmod(0o755)
            (root / '.codex').mkdir()
            (root / '.codex/config.toml').write_text('[mcp_servers.fabric]\ncommand="provenant"\n')
            environment = {**os.environ, 'HOME': directory, 'AGENT_FABRIC_INSTANCE_ROOT': str(root / 'instance'),
                           'PATH': str(bin_dir) + os.pathsep + os.environ['PATH']}
            for key in ['CODEX_HOME', 'CODEX_MCP_CONFIG', 'CLAUDE_CONFIG_DIR', 'CLAUDE_MCP_CONFIG']:
                environment.pop(key, None)
            def peer(path):
                return subprocess.run([str(ROOT / 'scripts/provenant'), 'peer'],
                    input=json.dumps({'protocol_version':1,'verb':'doctor','params':{'project_path':path}}),
                    capture_output=True, text=True, env=environment)
            response = peer('Repos/project')
            self.assertEqual(response.returncode, 0, response.stderr)
            value = json.loads(response.stdout)['result']
            self.assertEqual(value['host'], 'workshop')
            self.assertEqual(value['project'], {'path':'Repos/project','present':True})
            self.assertEqual(value['adapters']['claude']['executable'], str(claude))
            self.assertEqual(value['adapters']['claude']['signin']['status'], 'unusable')
            self.assertIn(value['adapters']['codex']['confinement']['status'], ['effective','degraded','unknown'])
            self.assertIsInstance(value['lane_temporary_paths']['fits'], bool)
            self.assertTrue(value['fabric_registrations']['codex']['registered'])
            self.assertFalse(json.loads(peer('Repos/missing').stdout)['result']['project']['present'])
            for path in ['/tmp', '../outside', 'Repos/../../outside']:
                response = peer(path)
                self.assertEqual(json.loads(response.stdout)['error']['code'], 'invalid_project_path')

    def test_doctor_loopback_uses_separate_home_and_ignores_original_command(self):
        with git_isolated_tempdir() as directory:
            root = Path(directory)
            local, remote = root / 'local', root / 'remote'
            for home, name in [(local, 'laptop'), (remote, 'workshop')]:
                (home / '.agents/.agent-fabric').mkdir(parents=True)
                (home / 'Repos/project').mkdir(parents=True)
                config = {'schema_version':1,'local_host':name}
                if name == 'laptop':
                    config['peers'] = {'workshop': {'ssh_destination':'workshop'}, 'offline':{'ssh_destination':'offline'},
                                       'slow': {'ssh_destination':'slow', 'response_deadline': .1}}
                (home / '.agents/.agent-fabric/hosts.json').write_text(json.dumps(config))
            (remote / '.local/bin').mkdir(parents=True)
            import shutil
            shutil.copy2(ROOT / 'scripts/provenant.template', remote / '.local/bin/provenant')
            (remote / '.agents/.agent-fabric/product-root.json').write_text(json.dumps({'schema_version':1,'product_root':str(ROOT)}))
            marker = root / 'injected'
            shim = root / 'ssh'
            shim.write_text('#!' + sys.executable + '\nimport os,sys\n' +
                'if sys.argv[-2] == "offline": sys.exit(255)\n' +
                'if sys.argv[-2] == "slow":\n import time; time.sleep(10)\n' +
                'os.environ["HOME"]=' + repr(str(remote)) + '\n' +
                'os.environ["AGENT_FABRIC_INSTANCE_ROOT"]=' + repr(str(remote / '.agents')) + '\n' +
                'os.environ["SSH_ORIGINAL_COMMAND"]=' + repr('touch ' + str(marker)) + '\n' +
                'os.chdir(os.environ["HOME"])\n' +
                'os.environ.pop("AGENT_FABRIC_PRODUCT_ROOT", None)\n' +
                'os.execv(".local/bin/provenant",["provenant","peer"])\n')
            shim.chmod(0o755)
            result = subprocess.run([str(ROOT / 'scripts/provenant'), 'hosts', 'doctor', '--json'],
                capture_output=True, text=True, cwd=local / 'Repos/project',
                env={**{key: value for key, value in os.environ.items() if not key.startswith('GIT_')},
                     'HOME':str(local),'AGENT_FABRIC_INSTANCE_ROOT':str(local / '.agents'),
                     'AGENT_FABRIC_SSH_PROGRAM':str(shim), 'GIT_CEILING_DIRECTORIES':str(root)})
            self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
            rows = {row['host']:row for row in json.loads(result.stdout)['hosts']}
            self.assertEqual(rows['laptop']['reachability'], 'reachable')
            self.assertEqual(rows['workshop']['reachability'], 'reachable')
            self.assertEqual(rows['workshop']['result']['project'], {'path':'Repos/project','present':True})
            self.assertEqual(rows['offline']['error']['code'], 'unreachable')
            self.assertEqual(rows['slow']['error']['code'], 'timeout')
            self.assertEqual(rows['slow']['reachability'], 'unreachable')
            self.assertFalse(marker.exists())


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
                 'fabric_registrations': {},
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

    def test_doctor_rejects_missing_or_malformed_diagnostic_fields_after_hello(self):
        import fabric_hosts as hosts
        valid = {'protocol_version':1, 'host':'workshop', 'revision':'fixture',
                 'project':{'path':'Repos/project','present':True}, 'adapters':{},
                 'fabric_registrations':{}, 'lane_temporary_paths':{'fits':False,'max_path_bytes':120,'limit_bytes':103}}
        malformed = [{}, {**valid, 'adapters':{'codex':{'executable':'codex','signin':{'status':'probably'},'confinement':{'status':'effective'}}}},
                     {**valid, 'fabric_registrations':{'codex':{'registered':'yes'}}},
                     {**valid, 'adapters':{'codex':{'executable':None,'signin':{'status':[]},'confinement':{'status':'effective'}}}},
                     {**valid, 'lane_temporary_paths':{'fits':True,'limit_bytes':'103'}}]
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
                '{"protocol_version":1,"host":"healthy","revision":None,"project":{"path":"Repos/project","present":True},"adapters":{},"fabric_registrations":{},"lane_temporary_paths":{"fits":True,"max_path_bytes":90,"limit_bytes":103}}\n'
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
            f'f=importlib.util.spec_from_file_location("fabric_hosts", {str(ROOT / "scripts/fabric_hosts.py")!r}); '
            'm=importlib.util.module_from_spec(f); sys.modules["fabric_hosts"]=m; f.loader.exec_module(m); '
            'import model_route, adapters; '
            'assert str(m.PRODUCT_ROOT / "scripts") in sys.path; '
            'assert str(m.PRODUCT_ROOT / "skills/orchestrate/scripts") in sys.path'
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
            return subprocess.run([str(ROOT / 'scripts/provenant'), 'hosts', 'list', '--json'],
                                  capture_output=True, text=True,
                                  env={**os.environ, 'AGENT_FABRIC_INSTANCE_ROOT': directory})

    def test_absent_configuration_is_local_only(self):
        result = self.command(None)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(json.loads(result.stdout)['hosts'], [{'host': 'local', 'local': True}])

    def test_instance_root_must_be_absolute(self):
        result = subprocess.run([str(ROOT / 'scripts/provenant'), 'hosts', 'list', '--json'],
            capture_output=True, text=True, env={**os.environ, 'AGENT_FABRIC_INSTANCE_ROOT': '.'})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout)['error']['code'], 'invalid_config')

    def test_peer_defaults_and_instance_host_name(self):
        result = self.command({'schema_version': 1, 'local_host': 'laptop',
                               'peers': {'workshop': {'ssh_destination': 'user@workshop'}}})
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        peer = json.loads(result.stdout)['hosts'][1]
        self.assertEqual(peer['host'], 'workshop')
        self.assertEqual(peer['connect_timeout'], 5)
        self.assertEqual(peer['response_deadline'], 15)
        self.assertEqual(peer['peer_command'], '.local/bin/provenant peer')

    def test_configuration_refuses_unsafe_and_unknown_values(self):
        base = {'schema_version': 1, 'local_host': 'laptop', 'peers': {}}
        bad = [{**base, 'schema_version': True}, {**base, 'extra': 1},
               {**base, 'local_host': 'Laptop'}, {**base, 'peers': {'laptop': {'ssh_destination': 'x'}}}]
        for destination in ['-oProxyCommand=true', 'workshop;true', '$(id)', 'a b', 'a\nb']:
            bad.append({**base, 'peers': {'workshop': {'ssh_destination': destination}}})
        for fields in [{'extra': 1}, {'connect_timeout': 0}, {'response_deadline': False},
                       {'peer_command': '.local/bin/provenant peer; true'},
                       {'peer_command': '../bin/provenant peer'}, {'response_deadline': 10**400}]:
            bad.append({**base, 'peers': {'workshop': {'ssh_destination': 'workshop', **fields}}})
        for config in bad:
            with self.subTest(config=config):
                result = self.command(config)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(json.loads(result.stdout)['error']['code'], 'invalid_config')


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
            shutil.copy2(ROOT / 'scripts/provenant.template', remote_home / '.local/bin/provenant')
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
                result = subprocess.run([str(ROOT / 'scripts/provenant'), 'hosts', 'doctor', '--json'],
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
