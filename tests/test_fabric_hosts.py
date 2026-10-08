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




class LaneLoopbackContracts(unittest.TestCase):
    """Real peer owners against two isolated stores, without launching a provider."""

    def setUp(self):
        scratch = ROOT / '.agent-run/scratch'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix='hosts-lanes-', dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.local_home = self.root / 'laptop'
        self.peer_home = self.root / 'workshop'
        self.workspace = self.local_home / 'Repos/project'
        self.peer_workspace = self.peer_home / 'Repos/project'
        self.offline = self.root / 'offline'
        for home, workspace, host in [(self.local_home, self.workspace, 'laptop'),
                                      (self.peer_home, self.peer_workspace, 'workshop')]:
            workspace.mkdir(parents=True)
            initialized = subprocess.run(['git', 'init', '-q', str(workspace)], capture_output=True, text=True)
            self.assertEqual(initialized.returncode, 0, initialized.stderr)
            state = home / '.agents/.agent-fabric'
            state.mkdir(parents=True)
            config = {'schema_version': 1, 'local_host': host}
            if host == 'laptop':
                config['peers'] = {'workshop': {'ssh_destination': 'workshop', 'response_deadline': 10}}
                config['projects'] = {'Repos/project': {'default_host': 'workshop'}}
            (state / 'hosts.json').write_text(json.dumps(config))
        self.ssh = self.root / 'ssh'
        self.ssh.write_text('#!' + sys.executable + '\nimport os,sys\n'
            + 'if os.path.exists(' + repr(str(self.offline)) + '): sys.exit(255)\n'
            + 'os.environ["HOME"]=' + repr(str(self.peer_home)) + '\n'
            + 'os.environ["AGENT_FABRIC_INSTANCE_ROOT"]=' + repr(str(self.peer_home / '.agents')) + '\n'
            + 'os.environ["AGENT_FABRIC_STATE_DIRECTORY"]=' + repr(str(self.peer_home / 'state')) + '\n'
            + 'os.chdir(os.environ["HOME"])\n'
            + 'os.execv(' + repr(str(ROOT / 'scripts/provenant')) + ',["provenant","peer"])\n')
        self.ssh.chmod(0o755)
        self.env = {**os.environ, 'HOME': str(self.local_home),
            'HARNESS_PYTHON': sys.executable, 'TMPDIR': str(self.root),
            'AGENT_FABRIC_INSTANCE_ROOT': str(self.local_home / '.agents'),
            'AGENT_FABRIC_STATE_DIRECTORY': str(self.local_home / 'state'),
            'AGENT_FABRIC_PRODUCT_ROOT': str(ROOT), 'AGENT_FABRIC_SSH_PROGRAM': str(self.ssh),
            'AGENT_FABRIC_SEAT': 'codex', 'PROVENANT_HOST_LOCAL_ONLY': '0'}
        self.local_run = self.terminal(self.workspace, 'shared-task', 'shared')
        self.peer_run = self.terminal(self.peer_workspace, 'shared-task', 'shared')

    def terminal(self, workspace, task_id, suffix, text='remote-result-' * 200):
        run_id = 'mcp-' + suffix
        directory = workspace / '.agent-run/runs' / ('20261008-0000-dispatch-fixture-' + suffix)
        attempt = directory / 'tasks' / task_id / 'attempt-001'
        attempt.mkdir(parents=True)
        row = json.loads((ROOT / 'runtime/fabric/tests/fixtures/attempt.json').read_text())
        # Synthetic fixture metadata does not attribute work to a guessed route.
        row.update(run_id=run_id, task_id=task_id, cwd=str(workspace), worktree=None,
            started_at='2026-10-08T00:00:00Z', ended_at='2026-10-08T00:00:01Z', pgid=None,
            provenance={'requested': {'adapter': 'fixture'}, 'resolved_model': 'fixture'},
            paths={'result': 'tasks/' + task_id + '/attempt-001/result.md'})
        (attempt / 'attempt.json').write_text(json.dumps(row))
        (attempt / 'result.md').write_text(text)
        return run_id

    def lanes(self, *selectors):
        process = subprocess.run([str(ROOT / 'scripts/provenant'), 'lanes', '--json', *selectors],
            cwd=self.workspace, env=self.env, capture_output=True, text=True, timeout=30)
        self.assertEqual(process.returncode, 0, process.stderr + process.stdout)
        return json.loads(process.stdout)

    def client(self, action, **request):
        process = subprocess.run([str(ROOT / 'scripts/fabric-hosts'), 'lane'],
            input=json.dumps({'action': action, 'cwd': str(self.workspace), 'input': request}),
            cwd=self.workspace, env=self.env, capture_output=True, text=True, timeout=30)
        self.assertEqual(process.returncode, 0, process.stderr + process.stdout)
        return json.loads(process.stdout)

    def peer(self, verb, **request):
        environment = {**self.env, 'HOME': str(self.peer_home),
            'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents'),
            'AGENT_FABRIC_STATE_DIRECTORY': str(self.peer_home / 'state')}
        process = subprocess.run([str(ROOT / 'scripts/provenant'), 'peer'],
            input=json.dumps({'protocol_version': 1, 'verb': verb,
                'params': {'project_path': 'Repos/project', 'input': request}}),
            cwd=self.peer_home, env=environment, capture_output=True, text=True, timeout=30)
        value = json.loads(process.stdout)
        self.assertEqual(process.returncode, 0 if value['ok'] else 1, process.stderr + process.stdout)
        return value

    def test_federated_lanes_and_colliding_selectors(self):
        view = self.lanes()
        self.assertEqual(view['schema'], 'fabric.runs.v2')
        self.assertEqual({row['host'] for row in view['runs']}, {'laptop', 'workshop'})
        self.assertTrue(all(row['state'] == 'terminal' for row in view['runs']))
        selected = self.lanes('shared-task@workshop')['runs']
        self.assertEqual(len(selected), 1)
        self.assertEqual(selected[0]['host'], 'workshop')
        ambiguous = self.client('lanes', ids=['shared-task'])
        self.assertEqual(ambiguous['error'], 'ambiguous_selector')
        status = self.client('status', ids=['shared-task@workshop'])
        self.assertEqual(status['status'], 'ok')
        self.assertEqual(status['runs'][0]['reachability'], 'reachable')

    def test_outage_preserves_snapshot_and_success_replaces_it(self):
        self.lanes()
        self.offline.touch()
        stale = self.lanes('shared-task@workshop')['runs'][0]
        self.assertEqual(stale['state'], 'unreachable')
        self.assertEqual(stale['last_known_state'], 'terminal')
        self.assertEqual(stale['last_known_status'], 'ok')
        self.assertGreaterEqual(stale['age_seconds'], 0)
        self.assertIsNone(stale['pgid_alive'])
        self.offline.unlink()
        row_path = next(self.peer_workspace.glob('.agent-run/runs/*/tasks/*/attempt-001/attempt.json'))
        row = json.loads(row_path.read_text())
        row['status'] = 'failed'
        row_path.write_text(json.dumps(row))
        fresh = self.lanes('shared-task@workshop')['runs'][0]
        self.assertEqual(fresh['status'], 'failed')
        self.assertEqual(fresh['reachability'], 'reachable')
        self.offline.touch()
        self.assertEqual(self.lanes('shared-task@workshop')['runs'][0]['last_known_status'], 'failed')

    def test_federated_caps_preserve_owner_omissions_and_explicit_selectors(self):
        for index in range(25):
            self.terminal(self.peer_workspace, f'extra-{index}', f'extra-{index}')
        view = self.lanes()
        self.assertEqual(len(view['runs']), 20)
        self.assertEqual(view['omitted'], 7)
        selected = self.lanes('extra-0@workshop')
        self.assertEqual(len(selected['runs']), 1)
        self.assertEqual(selected['omitted'], 0)

    def test_explicit_selectors_find_retained_terminal_history(self):
        path = next(self.peer_workspace.glob('.agent-run/runs/*/tasks/*/attempt-001/attempt.json'))
        row = json.loads(path.read_text())
        row['ended_at'] = '2026-01-01T00:00:00Z'
        path.write_text(json.dumps(row))
        self.assertFalse(any(row['host'] == 'workshop' for row in self.lanes()['runs']))
        self.assertEqual(self.lanes('shared-task@workshop')['runs'][0]['state'], 'terminal')
        self.assertEqual(self.client('status', ids=['shared-task@workshop'])['runs'][0]['state'], 'terminal')

    def test_routed_output_is_bounded_and_continues(self):
        first = self.client('output', id='shared-task@workshop', max_bytes=17)
        self.assertEqual(first['id'], 'shared-task@workshop')
        self.assertEqual(len(first['digest'].encode()), 17)
        self.assertEqual(first['next_offset'], 17)
        self.assertFalse(first['eof'])
        second = self.client('output', id='shared-task@workshop', offset=first['next_offset'], max_bytes=17)
        self.assertEqual(second['offset'], 17)
        self.assertEqual(second['next_offset'], 34)
        self.assertEqual(first['digest'] + second['digest'], ('remote-result-' * 200)[:34])

    def test_control_operation_retry_is_idempotent_and_conflicts_are_refused(self):
        request = {'id': self.peer_run, 'operation_id': 'cancel-repeat'}
        first = self.peer('cancel', **request)
        second = self.peer('cancel', **request)
        self.assertTrue(first['ok'])
        self.assertEqual(first, second)
        reconciled = self.peer('operation', operation_id='cancel-repeat')
        self.assertEqual(first, reconciled)
        conflicting = self.peer('cancel', **{**request, 'reason': 'different request'})
        self.assertFalse(conflicting['ok'])
        self.assertEqual(conflicting['error']['code'], 'operation_conflict')
        self.assertEqual(len(self.lanes('shared-task@workshop')['runs']), 1)

    def test_default_dispatch_fallback_is_only_before_send(self):
        self.offline.touch()
        default = self.client('dispatch', prompt='fixture, no provider launch')
        self.assertTrue(default['local'])
        self.assertEqual(default['fallback'], {'from_host': 'workshop', 'reason': 'unreachable'})
        self.assertIn('fallback', default['digest'])
        explicit = self.client('dispatch', host='workshop', prompt='fixture, no provider launch')
        self.assertEqual(explicit['status'], 'rejected')
        self.assertEqual(explicit['error'], 'unreachable')

    def test_remote_paths_outside_home_are_refused_before_launch(self):
        rejected = self.client('dispatch', host='workshop', prompt='fixture', cwd='/etc')
        self.assertEqual(rejected['error'], 'invalid_remote_path')
        response = self.peer('dispatch', operation_id='bad-path', prompt='fixture', cwd='/etc')
        self.assertFalse(response['ok'])
        self.assertEqual(response['error']['code'], 'invalid_remote_path')

    def test_prompt_credentials_and_hardlinks_never_cross_the_peer_boundary(self):
        credential = self.workspace / '.codex/auth.json'
        credential.parent.mkdir()
        credential.write_text('{"access_token":"private"}')
        rejected = self.client('dispatch', host='workshop', prompt_file=str(credential))
        self.assertEqual(rejected['error'], 'credential_or_auth_store_denied')
        original = self.workspace / 'prompt.md'
        original.write_text('ordinary input')
        linked = self.workspace / 'linked.md'
        os.link(original, linked)
        rejected = self.client('dispatch', host='workshop', prompt_file=str(linked))
        self.assertEqual(rejected['error'], 'prompt_hard_link_denied')
        target = self.workspace / 'regular.txt'
        target.write_text('private environment')
        link = self.workspace / '.env'
        link.symlink_to(target)
        rejected = self.client('dispatch', host='workshop', prompt_file=str(link))
        self.assertEqual(rejected['error'], 'credential_or_auth_store_denied')


    def test_prelaunch_owner_failure_is_a_durable_rejection(self):
        import fabric_hosts as hosts
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            federation = hosts.lane_owner()
            params = {'project_path': 'Repos/project', 'input': {
                'operation_id': 'prelaunch-failure', 'prompt': 'ordinary fixture input'}}
            with patch.object(federation, 'worker', side_effect=hosts.HostError(
                    'lane_owner_failed', 'fixture owner failed before recording a launch')) as worker:
                first = federation.peer_call('dispatch', params)
                repeated = federation.peer_call('dispatch', params)
                reconciled = federation.peer_call('operation', {
                    'project_path': 'Repos/project', 'input': {'operation_id': 'prelaunch-failure'}})
            self.assertEqual(first['status'], 'rejected')
            self.assertEqual(first['error'], 'lane_owner_failed')
            self.assertEqual(first, repeated)
            self.assertEqual(first, reconciled)
            self.assertEqual(worker.call_count, 1)

    def test_peer_operation_ledger_does_not_retain_confidential_prompt(self):
        import fabric_hosts as hosts
        marker = 'PRIVATE-PROMPT-ONLY-' + 'Q' * 300
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            federation = hosts.lane_owner()
            with patch.object(federation, 'worker', return_value={
                    'status': 'rejected', 'error': 'fixture_refusal', 'fix': 'fixture only'}):
                federation.peer_call('dispatch', {'project_path': 'Repos/project',
                    'input': {'operation_id': 'private-request', 'prompt': marker, 'confidential': True}})
            with federation.database() as database:
                request = database.execute('SELECT request FROM operations WHERE id=?',
                    ('private-request',)).fetchone()[0]
            self.assertNotIn(marker, request)
            self.assertNotIn(marker.encode(), (federation.root / 'hosts-state.sqlite3').read_bytes())
            # Idempotence remains request-bound: changing the private prompt conflicts.
            with self.assertRaises(hosts.HostError) as raised:
                federation.peer_call('dispatch', {'project_path': 'Repos/project',
                    'input': {'operation_id': 'private-request', 'prompt': marker + 'changed', 'confidential': True}})
            self.assertEqual(raised.exception.code, 'operation_conflict')

    def test_missing_cancel_operation_never_replays_on_a_later_attempt(self):
        import sqlite3
        from contextlib import closing
        self.lanes()
        state = self.local_home / '.agents/.agent-fabric/hosts-state.sqlite3'
        payload = json.dumps({'id': self.peer_run, 'operation_id': 'lost-cancel'})
        with closing(sqlite3.connect(state)) as database, database:
            database.execute('INSERT INTO pending VALUES (?,?,?,?,?)',
                ('lost-cancel', 'Repos/project', 'workshop', 'cancel', payload))
        attempt_path = next(self.peer_workspace.glob('.agent-run/runs/*/tasks/*/attempt-001/attempt.json'))
        later = json.loads(attempt_path.read_text())
        later.update(attempt=2, status='ok')
        later_dir = attempt_path.parent.parent / 'attempt-002'
        later_dir.mkdir()
        (later_dir / 'attempt.json').write_text(json.dumps(later))
        verbs = self.root / 'peer-verbs.jsonl'
        self.ssh.write_text('#!' + sys.executable + '\nimport os,sys,json,subprocess\n'
            + 'raw=sys.stdin.buffer.read()\n'
            + 'with open(' + repr(str(verbs)) + ',"a") as stream: stream.write(json.loads(raw)["verb"]+"\\n")\n'
            + 'os.environ["HOME"]=' + repr(str(self.peer_home)) + '\n'
            + 'os.environ["AGENT_FABRIC_INSTANCE_ROOT"]=' + repr(str(self.peer_home / '.agents')) + '\n'
            + 'os.environ["AGENT_FABRIC_STATE_DIRECTORY"]=' + repr(str(self.peer_home / 'state')) + '\n'
            + 'result=subprocess.run([' + repr(str(ROOT / 'scripts/provenant'))
            + ',"peer"],input=raw,capture_output=True,cwd=os.environ["HOME"])\n'
            + 'sys.stdout.buffer.write(result.stdout);sys.stderr.buffer.write(result.stderr);sys.exit(result.returncode)\n')
        view = self.client('lanes')
        self.assertEqual(view['status'], 'ok')
        self.assertIn('operation', verbs.read_text().splitlines())
        self.assertNotIn('cancel', verbs.read_text().splitlines())
        self.assertFalse((self.peer_workspace / '.agent-run/runs' /
            '20261008-0000-dispatch-fixture-shared/cancel').exists())

    def test_mode_aliases_use_canonical_per_mode_placement(self):
        config_path = self.local_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(config_path.read_text())
        config['projects']['Repos/project']['modes'] = {
            'read_only': 'laptop', 'worktree_write': 'laptop'}
        config_path.write_text(json.dumps(config))
        self.offline.touch()
        for mode in ['write', 'rw', 'worktree', 'read', 'ro']:
            with self.subTest(mode=mode):
                placed = self.client('dispatch', mode=mode, prompt='fixture placement only')
                self.assertTrue(placed['local'])
                self.assertNotIn('fallback', placed)

    def test_qualified_offline_selector_without_snapshot_is_unreachable(self):
        self.offline.touch()
        for action in ['lanes', 'status']:
            with self.subTest(action=action):
                view = self.client(action, ids=['never-observed@workshop'])
                self.assertEqual(view['status'], 'ok')
                self.assertEqual(len(view['runs']), 1)
                self.assertEqual(view['runs'][0]['id'], 'never-observed@workshop')
                self.assertEqual(view['runs'][0]['host'], 'workshop')
                self.assertEqual(view['runs'][0]['state'], 'unreachable')
                self.assertEqual(view['runs'][0]['reachability'], 'unreachable')

    def test_recovered_newer_attempt_replaces_snapshot_without_regressing_terminal(self):
        import fabric_hosts as hosts
        with patch.dict(os.environ, self.env):
            federation = hosts.lane_owner()
            old = {'id': self.peer_run, 'run_id': self.peer_run, 'task_id': 'shared-task',
                'attempt': 1, 'state': 'terminal', 'status': 'ok'}
            recovered = {**old, 'attempt': 2, 'state': 'running', 'status': 'running', 'session': 'named session'}
            def pending(identifier):
                with federation.database() as database:
                    database.execute('INSERT INTO pending VALUES (?,?,?,?,?)', (identifier, 'Repos/project', 'workshop',
                        'resume', json.dumps({'resume': self.peer_run, 'operation_id': identifier})))
            def response(verb, params, **kwargs):
                return hosts.envelope({'status': 'ok', 'runs': [recovered if verb == 'operation' else old]})
            pending('new-attempt')
            with patch.object(federation, 'worker', return_value={'status': 'ok', 'runs': []}), \
                    patch.object(hosts.PeerClient, 'call', side_effect=response):
                view = federation.reads('status', 'Repos/project', {'ids': ['shared-task@workshop']})
            self.assertEqual(view['runs'][0]['attempt'], 2)
            with patch.object(federation, 'worker', return_value={'status': 'ok', 'runs': []}), \
                    patch.object(hosts.PeerClient, 'call', return_value=hosts.envelope(error=hosts.HostError('unreachable', 'offline'))):
                stale = federation.reads('status', 'Repos/project', {'ids': ['shared-task@workshop']})
            self.assertEqual(stale['runs'][0]['attempt'], 2)
            self.assertEqual(stale['runs'][0]['last_known_state'], 'running')
            self.assertEqual(stale['runs'][0]['session'], 'named session@workshop')
            # A cached launch response for the same attempt cannot overwrite a
            # full read that has already observed its terminal outcome.
            old.update(attempt=2)
            pending('same-attempt')
            with patch.object(federation, 'worker', return_value={'status': 'ok', 'runs': []}), \
                    patch.object(hosts.PeerClient, 'call', side_effect=response):
                final = federation.reads('status', 'Repos/project', {'ids': ['shared-task@workshop']})
            self.assertEqual(final['runs'][0]['state'], 'terminal')

    def test_unrecognised_bare_control_selector_reaches_local_owner(self):
        import fabric_hosts as hosts
        with patch.dict(os.environ, self.env):
            federation = hosts.lane_owner()
            with patch.object(federation, 'reads', side_effect=hosts.HostError('selector_not_found', 'no matching row')):
                result = federation.client_call('cancel', str(self.workspace), {'id': '.agent-run/runs/relative-run'})
            self.assertTrue(result['local'])
            self.assertEqual(result['input']['id'], '.agent-run/runs/relative-run')

    def test_missing_resume_handoff_and_session_operations_never_replay(self):
        import fabric_hosts as hosts
        with patch.dict(os.environ, self.env):
            federation = hosts.lane_owner()
            for verb, payload in [('resume', {'resume': self.peer_run}), ('handoff', {'handoff': self.peer_run}),
                    ('dispatch', {'session': 'named session', 'prompt': 'continue'})]:
                identifier = f'lost-{verb}'
                with federation.database() as database:
                    database.execute('INSERT INTO pending VALUES (?,?,?,?,?)',
                        (identifier, 'Repos/project', 'workshop', verb, json.dumps(payload)))
            def response(verb, params, **kwargs):
                return hosts.envelope({'status': 'operation_missing' if verb == 'operation' else 'ok', 'runs': []})
            with patch.object(federation, 'worker', return_value={'status': 'ok', 'runs': []}), \
                    patch.object(hosts.PeerClient, 'call', side_effect=response) as peer:
                view = federation.reads('lanes', 'Repos/project', {})
            self.assertTrue(all(call.args[0] in {'lanes', 'operation'} for call in peer.call_args_list))
            self.assertEqual({row.get('error') for row in view['runs']}, {'resume_not_sent', 'handoff_not_sent', 'dispatch_not_sent'})

    def fixture_product(self):
        """Reuse existing owner fixtures, never dispatch a real provider."""
        import shutil
        product = self.root / 'product'
        owners = product / 'skills/orchestrate/scripts'
        helpers = product / 'scripts/lib'
        owners.mkdir(parents=True)
        helpers.mkdir(parents=True)
        (product / 'config').mkdir()
        for name in ['model-routing.json', 'adapter-compatibility.yaml']:
            shutil.copyfile(ROOT / 'config' / name, product / 'config' / name)
        shutil.copyfile(ROOT / 'scripts/lib/harness-python.sh', helpers / 'harness-python.sh')
        fixture = ROOT / 'runtime/fabric/tests/v2-owner-fixture.mjs'
        node = shutil.which('node')
        for name in ['run_dir_init.sh', 'dispatch_run.py', 'run_controls.py']:
            target = owners / name
            if name.endswith('.sh'):
                target.write_text('#!/bin/sh\nPROVENANT_FIXTURE_OWNER=' + shlex.quote(name)
                    + ' exec ' + shlex.quote(node) + ' ' + shlex.quote(str(fixture)) + ' "$@"\n')
            else:
                if name == 'run_controls.py':
                    target.write_text('#!' + sys.executable + '\nimport sys,json,time\nfrom pathlib import Path\n'
                        + 'args=sys.argv[1:]\nroot=Path(args[args.index("--run-dir")+1])\n(root/"cancel").touch()\n'
                        + 'deadline=time.monotonic()+5\n'
                        + 'while time.monotonic()<deadline:\n'
                        + ' rows=[json.loads(path.read_text()) for path in root.glob("tasks/*/attempt-*/attempt.json")]\n'
                        + ' if rows and all(row["state"]=="terminal" for row in rows):sys.exit(0)\n'
                        + ' time.sleep(.02)\nsys.exit(1)\n')
                else:
                    target.write_text('#!' + sys.executable + '\nimport os,sys\n'
                        + 'os.environ["PROVENANT_FIXTURE_OWNER"]=' + repr(name) + '\n'
                        + 'os.execv(' + repr(node) + ',[' + repr(node) + ',' + repr(str(fixture)) + ',*sys.argv[1:]])\n')
            target.chmod(0o755)
        self.ssh.write_text('#!' + sys.executable + '\nimport os,sys,subprocess,json\n'
            + 'request=sys.stdin.buffer.read()\n'
            + 'environment={**os.environ,"HOME":' + repr(str(self.peer_home))
            + ',"AGENT_FABRIC_INSTANCE_ROOT":' + repr(str(self.peer_home / '.agents'))
            + ',"AGENT_FABRIC_PRODUCT_ROOT":' + repr(str(product))
            + ',"AGENT_FABRIC_STATE_DIRECTORY":' + repr(str(self.peer_home / 'state')) + '}\n'
            + 'result=subprocess.run([' + repr(str(ROOT / 'scripts/fabric-hosts'))
            + ',"peer"],input=request,capture_output=True,env=environment,cwd=environment["HOME"])\n'
            + 'if json.loads(request)["verb"] == "dispatch" and os.path.exists(' + repr(str(self.root / 'drop')) + '): sys.exit(255)\n'
            + 'sys.stdout.buffer.write(result.stdout)\nsys.stderr.buffer.write(result.stderr)\nsys.exit(result.returncode)\n')
        return product

    def test_lost_remote_launch_reconciles_once_and_owner_survives_peer_exit(self):
        self.fixture_product()
        (self.root / 'drop').touch()
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='slow', task_id='detached', operation_id='once')
        self.assertEqual(launched['status'], 'launch_unknown')
        try:
            view = self.lanes()
            remote = [row for row in view['runs'] if row.get('task_id') == 'detached@workshop']
            self.assertEqual(len(remote), 1, view)
            self.assertTrue(remote[0]['pgid_alive'])
            self.assertEqual(remote[0]['state'], 'running')
            (self.root / 'drop').unlink()
            repeated = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
                prompt='slow', task_id='detached', operation_id='once')
            self.assertEqual(repeated['status'], 'running')
            self.assertEqual(len([row for row in self.lanes()['runs'] if row.get('task_id') == 'detached@workshop']), 1)
            cancelled = self.client('cancel', id='detached@workshop', operation_id='stop-once')
            self.assertEqual(cancelled['runs'][0]['status'], 'cancelled', cancelled)
        finally:
            # Fixture cancellation is cooperative and runs on the owning host.
            self.client('cancel', id='detached@workshop', operation_id='cleanup-stop')

    def test_unreachable_wait_times_out_and_names_the_host(self):
        self.lanes()
        self.offline.touch()
        result = subprocess.run([str(ROOT / 'scripts/provenant'), 'lanes', '--wait', '--all', '--timeout', '0', 'shared-task@workshop'],
            cwd=self.workspace, env=self.env, capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
        self.assertIn('unreachable hosts: workshop', result.stdout)


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
