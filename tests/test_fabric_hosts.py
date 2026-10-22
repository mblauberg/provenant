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
        self.assertEqual(value['result']['host'], default_host())
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
            self.assertTrue(value['python']['supported'])
            self.assertEqual(value['python']['minimum'], '3.11')
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
            return subprocess.run([str(ROOT / 'scripts/provenant'), 'hosts', 'list', '--json'],
                                  capture_output=True, text=True,
                                  env={**os.environ, 'AGENT_FABRIC_INSTANCE_ROOT': directory})

    def test_absent_configuration_is_local_only(self):
        result = self.command(None)
        self.assertEqual(result.returncode, 0, result.stderr + result.stdout)
        self.assertEqual(json.loads(result.stdout)['hosts'], [{'host': default_host(), 'local': True}])

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
                self.assertEqual(result.returncode, 0)
                value = json.loads(result.stdout)
                self.assertEqual(value['error']['code'], 'invalid_config')
                # The local row keeps a valid local_host even when another field is invalid.
                self.assertEqual(value['hosts'], [{'host': default_host(config), 'local': True}])




class HostDiagnosticsContracts(unittest.TestCase):
    def test_invalid_configuration_keeps_local_row_and_visible_note(self):
        with git_isolated_tempdir() as directory:
            root = Path(directory)
            (root / 'project').mkdir()
            config = root / 'instance/.agent-fabric'
            config.mkdir(parents=True)
            (config / 'hosts.json').write_text('{broken')
            environment = {**os.environ, 'HOME': directory,
                           'AGENT_FABRIC_INSTANCE_ROOT': str(root / 'instance')}
            for action in ('list', 'doctor'):
                for json_output in (False, True):
                    with self.subTest(action=action, json=json_output):
                        command = [str(ROOT / 'scripts/provenant'), 'hosts', action]
                        if json_output:
                            command.append('--json')
                        result = subprocess.run(command, capture_output=True, text=True,
                                                cwd=root / 'project', env=environment)
                        self.assertEqual(result.returncode, 1 if action == 'doctor' else 0,
                                         result.stdout + result.stderr)
                        if json_output:
                            value = json.loads(result.stdout)
                            self.assertEqual([row['host'] for row in value['hosts']], [default_host()])
                            self.assertEqual(value['error']['code'], 'invalid_config')
                        else:
                            self.assertIn(default_host() + ' ', result.stdout)
                            self.assertIn('invalid_config', result.stdout)

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



class LaneLoopbackContracts(unittest.TestCase):
    """Real peer owners against two isolated stores, without launching a provider."""

    def setUp(self):
        scratch = ROOT / '.agent-run/scratch'
        scratch.mkdir(parents=True, exist_ok=True)
        self.temporary = tempfile.TemporaryDirectory(prefix='hosts-lanes-', dir=scratch)
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.addCleanup(self._cleanup_peer_lane_groups)
        # In-process transport uses tempfile while HOME/TMPDIR are patched.
        # Keep its cached default owned by this fixture and restore it before cleanup.
        temporary_default = patch.object(tempfile, 'tempdir', str(self.root))
        temporary_default.start()
        self.addCleanup(temporary_default.stop)
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

    def _peer_lane_groups(self):
        result = subprocess.run(['ps', '-ww', '-axo', 'pid=,pgid=,command='], capture_output=True,
            text=True, check=True)
        groups = {}
        for line in result.stdout.splitlines():
            fields = line.strip().split(None, 2)
            if len(fields) == 3 and 'peer-lanes.ts' in fields[2] and str(self.root) in fields[2]:
                groups[int(fields[1])] = fields[2]
        return groups

    def _cleanup_peer_lane_groups(self):
        groups = self._peer_lane_groups()
        for group in groups:
            try:
                os.killpg(group, signal.SIGTERM)
            except ProcessLookupError:
                pass
        if groups:
            time.sleep(0.1)
        for group in groups:
            try:
                os.killpg(group, signal.SIGKILL)
            except OSError:
                pass
        self.assertFalse(groups, f'peer-lanes.ts process groups outlived the test: {groups}')

    def test_peer_lane_leak_guard_fails_and_reaps_process_group(self):
        child_pid_path = self.root / 'peer-lane-child.pid'
        child_code = 'import time; time.sleep(60)'
        parent_code = ('import subprocess, sys, time; from pathlib import Path; '
            f'child = subprocess.Popen([sys.executable, "-c", {child_code!r}]); '
            f'Path({str(child_pid_path)!r}).write_text(str(child.pid)); time.sleep(60)')
        process = subprocess.Popen([sys.executable, '-c', parent_code,
            str(self.root / 'runtime/fabric/src/peer-lanes.ts')], start_new_session=True)
        try:
            deadline = time.monotonic() + 2
            while not child_pid_path.exists() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(child_pid_path.exists(), 'peer-lane fixture child did not start')
            with self.assertRaisesRegex(AssertionError, 'peer-lanes.ts'):
                self._cleanup_peer_lane_groups()
            process.wait(timeout=2)
            child_pid = int(child_pid_path.read_text())
            deadline = time.monotonic() + 2
            while time.monotonic() < deadline:
                try:
                    os.kill(child_pid, 0)
                except ProcessLookupError:
                    break
                time.sleep(0.01)
            else:
                self.fail(f'peer-lane process group left child {child_pid} alive')
        finally:
            if process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except OSError:
                    pass
                process.wait(timeout=2)

    def terminal(self, workspace, task_id, suffix, text='remote-result-' * 200):
        run_id = 'mcp-' + suffix
        # Relative to now: the default lanes view hides terminal rows older than a day.
        now = time.time()
        stamp = lambda seconds: time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime(seconds))
        directory = workspace / '.agent-run/runs' / (
            time.strftime('%Y%m%d-%H%M', time.gmtime(now - 60)) + '-dispatch-fixture-' + suffix)
        attempt = directory / 'tasks' / task_id / 'attempt-001'
        attempt.mkdir(parents=True)
        row = json.loads((ROOT / 'runtime/fabric/tests/fixtures/attempt.json').read_text())
        # Synthetic fixture metadata does not attribute work to a guessed route.
        row.update(run_id=run_id, task_id=task_id, cwd=str(workspace), worktree=None,
            started_at=stamp(now - 60), ended_at=stamp(now - 59), pgid=None,
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

    def mcp(self, name, arguments, seat='codex'):
        client_module = ROOT / 'node_modules/@modelcontextprotocol/sdk/dist/esm/client/index.js'
        transport_module = ROOT / 'node_modules/@modelcontextprotocol/sdk/dist/esm/client/stdio.js'
        script = ('import {Client} from ' + json.dumps(client_module.as_uri()) + ';\n'
            + 'import {StdioClientTransport} from ' + json.dumps(transport_module.as_uri()) + ';\n'
            + 'const client=new Client({name:"writer-contract",version:"1"});\n'
            + 'try {await client.connect(new StdioClientTransport({command:process.execPath,args:'
            + json.dumps(['--import', str(ROOT / 'node_modules/tsx/dist/loader.mjs'), str(ROOT / 'runtime/fabric/src/server.ts')])
            + ',env:process.env,stderr:"pipe"}));\n'
            + 'const result=await client.callTool(' + json.dumps({'name': name, 'arguments': arguments}) + ');\n'
            + 'console.log(JSON.stringify(result.structuredContent ?? JSON.parse(result.content[0].text)));\n'
            + '}finally{await client.close();}')
        process = subprocess.run(['node', '--input-type=module', '-e', script], cwd=self.workspace,
            env={**self.env, 'AGENT_FABRIC_SEAT': seat}, capture_output=True, text=True, timeout=40)
        self.assertEqual(process.returncode, 0, process.stdout + process.stderr)
        return json.loads(process.stdout)

    def writer_fixture(self):
        self.fixture_product()
        config_path = self.peer_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(config_path.read_text())
        config['projects'] = {'Repos/project': {}}
        config_path.write_text(json.dumps(config))
        def git(path, *args):
            result = subprocess.run(['git', '-C', str(path), *args], capture_output=True, text=True,
                env={**os.environ, 'GIT_AUTHOR_NAME': 'Fixture', 'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
                    'GIT_COMMITTER_NAME': 'Fixture', 'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'})
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout.strip()
        (self.workspace / 'code.txt').write_text('committed base\n')
        git(self.workspace, 'add', 'code.txt')
        git(self.workspace, 'commit', '-qm', 'fixture base')
        base = git(self.workspace, 'rev-parse', 'HEAD')
        local = self.workspace / '.worktrees/feat-writer'
        git(self.workspace, 'worktree', 'add', '-b', 'feat/writer', str(local), base)
        (local / 'code.txt').write_text('uncommitted chair edit\n')
        return git, base, local

    def set_peer_response_deadline(self, seconds):
        path = self.local_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(path.read_text())
        config['peers']['workshop']['response_deadline'] = seconds
        path.write_text(json.dumps(config))

    def test_read_only_peer_uses_chair_head_in_detached_worktree_and_cleans_up(self):
        git, _, local = self.writer_fixture()
        (local / 'code.txt').write_text('chair committed review input\n')
        git(local, 'add', 'code.txt')
        git(local, 'commit', '-qm', 'chair review head')
        head = git(local, 'rev-parse', 'HEAD')
        brief = self.root / 'scratch-review.md'
        brief.write_text('slow')
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt_file=str(brief), mode='read_only', cwd=str(local),
            task_id='review', operation_id='review-head')
        self.assertEqual(launched['status'], 'running', launched)
        context = launched['code_transport']['readers'][0]
        remote = Path(context['worktree_path'])
        try:
            self.assertEqual(git(remote, 'rev-parse', 'HEAD'), head)
            self.assertEqual((remote / 'code.txt').read_text(), 'chair committed review input\n')
            self.assertNotEqual(remote, self.peer_workspace)
            symbolic = subprocess.run(['git', '-C', str(remote), 'symbolic-ref', '-q', 'HEAD'], capture_output=True)
            self.assertNotEqual(symbolic.returncode, 0)
            rows = self.client('status', ids=['review@workshop'], detail='full')['runs']
            self.assertEqual(rows[0]['cwd'], str(remote))
            import fabric_hosts as hosts
            with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                    'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
                hosts.lane_owner().code().cleanup_readers([{**rows[0], 'state': 'terminal',
                    'run_id': launched['id'].rsplit('@', 1)[0], 'task_id': 'review', 'pgid_alive': True}])
                self.assertTrue(remote.exists(), 'a live sequence owner still needs its frozen input')
        finally:
            self.client('cancel', id='review@workshop', operation_id='stop-review-head')
        self.wait_writer(launched['id'])
        self.assertFalse(remote.exists())
        self.assertNotIn(str(remote), git(self.peer_workspace, 'worktree', 'list', '--porcelain'))

    def test_reader_polling_preserves_input_between_fallback_attempts(self):
        self.writer_fixture()
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fallback-gap', task_id='reader-gap', operation_id='reader-gap')
        remote = Path(launched['code_transport']['readers'][0]['worktree_path'])
        run_dir = Path(launched['paths']['run_dir'])
        attempt = run_dir / 'tasks/reader-gap/attempt-001/attempt.json'
        try:
            deadline = time.monotonic() + 10
            while not attempt.exists() and time.monotonic() < deadline:
                time.sleep(.02)
            self.assertEqual(json.loads(attempt.read_text())['state'], 'terminal')
            row = self.client('lanes', ids=[launched['id']])['runs'][0]
            self.assertTrue(row['pgid_alive'], row)
            self.assertTrue(remote.exists(), 'the fallback owner still needs its input')
        finally:
            (run_dir / 'release').touch()
        self.wait_writer(launched['id'])
        self.assertFalse(remote.exists())

    def test_terminal_reader_resume_recreates_frozen_checkout(self):
        git, base, _ = self.writer_fixture()
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', task_id='reader-resume', operation_id='reader-first')
        self.assertEqual(launched['status'], 'running', launched)
        self.wait_writer(launched['id'])
        target = Path(launched['code_transport']['readers'][0]['worktree_path'])
        self.assertFalse(target.exists())
        resumed = self.client('resume', resume=launched['id'], prompt='slow', operation_id='reader-next')
        self.assertNotEqual(resumed.get('status'), 'rejected', resumed)
        try:
            target = Path(resumed['code_transport']['readers'][0]['worktree_path'])
            self.assertEqual(git(target, 'rev-parse', 'HEAD'), base)
            # Reach the production Python owner, which the provider fixture replaces.
            import fabric_hosts as hosts
            prior_file = Path(launched['paths']['run_dir']) / 'tasks/reader-resume/attempt-001/attempt.json'
            prior = json.loads(prior_file.read_text())
            prior['provenance']['resolved_model'] = 'fixture'
            prior['provenance']['effort_applied'] = None
            copied_run = self.root / 'resume-owner-check'
            copied = copied_run / 'tasks/reader-resume/attempt-001/attempt.json'
            copied.parent.mkdir(parents=True)
            copied.write_text(json.dumps(prior))
            (copied_run / 'RUN_RECEIPT.json').write_bytes((Path(launched['paths']['run_dir']) / 'RUN_RECEIPT.json').read_bytes())
            with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                    'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents'), 'PROVENANT_REMOTE_LANE': '1'}):
                owner = hosts._load_module('provenant_lane_prompt_reader', ROOT / 'skills/orchestrate/scripts/dispatch_run.py')
                args = owner.parser().parse_args(['--run-dir', str(copied_run), '--resume', prior['run_id'],
                    '--task-id', 'reader-resume', '--cwd', str(target), '--prompt-stdin'])
                owner.prepare_resume(args)
                self.assertEqual(args.provider_cwd, target)
        finally:
            self.client('cancel', id=launched['id'], operation_id='stop-reader-resume')
        self.wait_writer(launched['id'])
        self.assertFalse(target.exists())

    def test_read_only_creation_refuses_escaping_worktree_root(self):
        git, _, _ = self.writer_fixture()
        outside = self.peer_home / 'outside'
        outside.mkdir()
        (self.peer_workspace / '.worktrees').symlink_to(outside, target_is_directory=True)
        launched = self.client('dispatch', adapter='codex', model='fixture',
            prompt='slow', task_id='reader-boundary', operation_id='reader-boundary')
        try:
            self.assertTrue(launched.get('local'), launched)
            self.assertTrue(launched['fallback']['reason'])
            self.assertEqual(list(outside.iterdir()), [])
        finally:
            if launched.get('id'):
                self.client('cancel', id=launched['id'], operation_id='stop-reader-boundary')

    def test_reader_resume_recovers_frozen_input_when_launch_result_was_lost(self):
        import sqlite3
        git, base, _ = self.writer_fixture()
        first = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', task_id='reader-lost-result', operation_id='reader-lost-result')
        with closing(sqlite3.connect(self.peer_home / '.agents/.agent-fabric/hosts-state.sqlite3')) as database, database:
            database.execute('UPDATE operations SET result=NULL WHERE id=?', ('reader-lost-result',))
        self.wait_writer(first['id'])
        resumed = self.client('resume', resume=first['id'], prompt='slow', operation_id='reader-recovered')
        try:
            rows = self.client('status', ids=[first['id']], detail='full')['runs']
            target = Path(rows[0]['cwd'])
            self.assertEqual(git(target, 'rev-parse', 'HEAD'), base)
        finally:
            self.client('cancel', id=first['id'], operation_id='stop-reader-recovered')
        self.wait_writer(first['id'])
        self.assertFalse(target.exists())

    def test_remote_named_reader_session_uses_new_frozen_input_on_second_turn(self):
        git, base, _ = self.writer_fixture()
        first = self.client('dispatch', host='workshop', session='reader-session@workshop',
            adapter='codex', model='fixture', prompt='fixture only', operation_id='session-reader-first')
        self.wait_writer(first['id'])
        self.assertEqual(first['code_transport']['readers'][0]['base_revision'], base)
        (self.workspace / 'code.txt').write_text('new committed session input\n')
        git(self.workspace, 'add', 'code.txt')
        git(self.workspace, 'commit', '-qm', 'second session input')
        next_base = git(self.workspace, 'rev-parse', 'HEAD')
        self.assertNotEqual(next_base, base)
        second = self.client('dispatch', host='workshop', session='reader-session@workshop',
            prompt='slow', operation_id='session-reader-next')
        try:
            self.assertNotEqual(second.get('status'), 'rejected', second)
            row = self.client('status', ids=[first['id']], detail='full')['runs'][0]
            target = Path(row['cwd'])
            self.assertEqual(git(target, 'rev-parse', 'HEAD'), next_base)
            self.assertEqual((target / 'code.txt').read_text(), 'new committed session input\n')
            self.assertEqual(target, Path(second['code_transport']['readers'][0]['worktree_path']))
        finally:
            self.client('cancel', id=first['id'], operation_id='stop-session-reader')
        self.wait_writer(first['id'])
        self.assertFalse(target.exists())

    def test_reader_handoffs_use_independent_frozen_checkouts(self):
        git, base, _ = self.writer_fixture()
        first = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', task_id='handoff-reader', operation_id='handoff-input')
        self.wait_writer(first['id'])
        handoffs = []
        try:
            for operation in ('reader-handoff-a', 'reader-handoff-b'):
                value = self.client('handoff', handoff=first['id'], prompt='slow', operation_id=operation)
                handoffs.append(value)
                self.assertNotEqual(value.get('status'), 'rejected', value)
            targets = []
            for value in handoffs:
                rows = self.client('status', ids=[value['id']], detail='full')['runs']
                target = Path(rows[0]['cwd'])
                self.assertEqual(git(target, 'rev-parse', 'HEAD'), base)
                self.assertNotEqual(target, self.peer_workspace)
                targets.append(target)
            self.assertNotEqual(targets[0], targets[1])
            self.client('cancel', id=handoffs[0]['id'], operation_id='stop-reader-handoff-a')
            self.wait_writer(handoffs[0]['id'])
            self.assertFalse(targets[0].exists())
            self.assertTrue(targets[1].exists())
        finally:
            for index, value in enumerate(handoffs):
                self.client('cancel', id=value['id'], operation_id='cleanup-reader-handoff-' + str(index))
        self.wait_writer(handoffs[-1]['id'])
        self.assertFalse(targets[-1].exists())

    def test_mixed_batch_cleans_unnamed_reader_and_preserves_writer(self):
        _, _, local = self.writer_fixture()
        launched = self.client('dispatch', host='workshop', operation_id='mixed-input', tasks=[
            {'id': 'writer', 'adapter': 'codex', 'model': 'fixture', 'prompt': 'fixture only',
             'mode': 'worktree_write', 'worktree': str(local)},
            {'adapter': 'codex', 'model': 'fixture', 'prompt': 'fixture only', 'mode': 'read_only'}])
        self.assertEqual(launched['status'], 'running', launched)
        reader = Path(launched['code_transport']['readers'][0]['worktree_path'])
        writer = Path(launched['code_transport']['writers'][0]['worktree_path'])
        contexts = self.peer_home / '.agents/.agent-fabric/reader-contexts/mixed-input.json'
        if contexts.exists():
            self.assertTrue(all(context['read_only'] for context in json.loads(contexts.read_text())))
        self.wait_writer(launched['id'])
        self.assertFalse(reader.exists())
        self.assertFalse(contexts.exists())
        self.assertTrue(writer.exists())

    def test_read_only_transport_failure_records_local_fallback(self):
        result = self.client('dispatch', mode='read_only', prompt='review',
            operation_id='no-committed-input')
        self.assertTrue(result.get('local'), result)
        self.assertIn(result['fallback']['reason'], {'project_not_configured', 'code_transport_failed'})
        self.assertIn('fallback', result['digest'])
        import fabric_hosts as hosts
        with patch.dict(os.environ, self.env):
            with self.assertRaises(hosts.HostError) as error:
                hosts.lane_owner().client_call('dispatch', str(self.workspace), {
                    'mode': 'read_only', 'prompt': 'review', 'operation_id': 'no-committed-input'})
            self.assertEqual(error.exception.code, 'operation_conflict')

    def test_reader_chunk_failure_before_launch_falls_back_and_binds_locally(self):
        self.writer_fixture()
        import fabric_hosts as hosts
        with patch.dict(os.environ, self.env):
            owner = hosts.lane_owner()
            with patch.object(type(owner.code()), 'send', side_effect=hosts.HostError('timeout', 'chunk deadline')):
                result = owner.client_call('dispatch', str(self.workspace), {
                    'prompt': 'review', 'operation_id': 'reader-chunk-failure'})
            self.assertTrue(result.get('local'), result)
            self.assertEqual(result['fallback']['reason'], 'timeout')
            with owner.database() as database:
                self.assertIsNone(database.execute('SELECT id FROM pending WHERE id=?', ('reader-chunk-failure',)).fetchone())
                self.assertEqual(database.execute('SELECT host FROM writer_dispatches WHERE id=?',
                    ('reader-chunk-failure',)).fetchone()[0], 'laptop')
        self.assertFalse((self.peer_home / '.agents/.agent-fabric/operations/reader-chunk-failure.json').exists())

    def test_explicit_reader_host_never_falls_back_on_transport_failure(self):
        import fabric_hosts as hosts
        with patch.dict(os.environ, self.env):
            owner = hosts.lane_owner()
            with self.assertRaises(hosts.HostError):
                owner.client_call('dispatch', str(self.workspace), {
                    'host': 'workshop', 'mode': 'read_only', 'prompt': 'review',
                    'operation_id': 'explicit-reader-failure'})
            with owner.database() as db:
                self.assertIsNone(db.execute('SELECT id FROM writer_dispatches WHERE id=?',
                    ('explicit-reader-failure',)).fetchone())

    def test_explicit_reader_host_never_falls_back_after_chunk_timeout(self):
        import fabric_hosts as hosts
        self.writer_fixture()
        original_call = hosts.PeerClient.call
        def call(client, verb, *args, **kwargs):
            if verb == 'git-upload':
                raise hosts.HostError('timeout', 'chunk deadline')
            return original_call(client, verb, *args, **kwargs)
        with patch.dict(os.environ, self.env), patch.object(hosts.PeerClient, 'call', call):
            owner = hosts.lane_owner()
            result = owner.client_call('dispatch', str(self.workspace), {
                'host': 'workshop', 'prompt': 'review', 'operation_id': 'explicit-chunk'})
            self.assertNotIn('local', result)
            self.assertEqual(result['status'], 'launch_unknown')
            with owner.database() as db:
                self.assertEqual(db.execute('SELECT host FROM writer_dispatches WHERE id=?',
                    ('explicit-chunk',)).fetchone()[0], 'workshop')
        self.assertFalse((self.peer_home / '.agents/.agent-fabric/operations/explicit-chunk.json').exists())

    def test_explicit_reader_host_never_falls_back_after_peer_preparation_failure(self):
        self.writer_fixture()
        outside = self.peer_home / 'outside'
        outside.mkdir()
        (self.peer_workspace / '.worktrees').symlink_to(outside, target_is_directory=True)
        result = self.client('dispatch', host='workshop', prompt='review',
            operation_id='explicit-peer-prepare')
        self.assertNotIn('local', result)
        self.assertEqual(result['status'], 'rejected')
        self.assertTrue(result['code_transport_prelaunch_failure'])
        self.assertEqual(list(outside.iterdir()), [])

    def test_knob_overrides_are_refused_for_a_remote_lane_and_kept_for_a_local_one(self):
        import fabric_hosts as hosts
        request = {'mode': 'read_only', 'prompt': 'review', 'knobs': ['effort.shift=1']}
        with patch.dict(os.environ, self.env):
            owner = hosts.lane_owner()
            with self.assertRaises(hosts.HostError) as error:
                owner.client_call('dispatch', str(self.workspace), {**request, 'operation_id': 'knob-remote'})
            self.assertEqual(error.exception.code, 'knob_remote_unsupported')
            self.assertIn('--host laptop', str(error.exception))
            local = owner.client_call('dispatch', str(self.workspace), {**request, 'host': 'laptop'})
            self.assertTrue(local['local'])
            self.assertEqual(local['input']['knobs'], ['effort.shift=1'])

    def test_reader_cwd_outside_checkout_returns_typed_failure_and_retires_preparation(self):
        import fabric_hosts as hosts
        self.writer_fixture()
        outside = self.local_home / 'outside'
        outside.mkdir()
        (outside / '.git').write_text('gitdir: ' + str(outside / 'missing') + '\n')
        with patch.dict(os.environ, self.env):
            owner = hosts.lane_owner()
            request = {'mode': 'read_only', 'cwd': str(outside), 'prompt': 'review'}
            with self.assertRaises(hosts.HostError) as error:
                owner.client_call('dispatch', str(self.workspace), {**request,
                    'host': 'workshop', 'operation_id': 'outside-explicit'})
            self.assertEqual(error.exception.code, 'invalid_project_path')
            self.assertIn(str(outside), str(error.exception))
            with owner.database() as db:
                self.assertIsNone(db.execute('SELECT id FROM writer_dispatches WHERE id=?',
                    ('outside-explicit',)).fetchone())
            with self.assertRaises(hosts.HostError) as implicit:
                owner.client_call('dispatch', str(self.workspace), {**request, 'operation_id': 'outside-implicit'})
            self.assertEqual(implicit.exception.code, 'invalid_project_path')

    def dispatch_from(self, chair, **request):
        process = subprocess.run([str(ROOT / 'scripts/fabric-hosts'), 'lane'],
            input=json.dumps({'action': 'dispatch', 'cwd': str(chair), 'input': request}),
            cwd=chair, env=self.env, capture_output=True, text=True, timeout=30)
        return json.loads(process.stdout)

    def test_uniform_per_task_host_places_the_batch_and_mixed_hosts_are_refused(self):
        self.writer_fixture()
        path = self.local_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(path.read_text())
        config['projects'] = {'Repos/project': {}}
        path.write_text(json.dumps(config))
        base = dict(adapter='codex', model='fixture', mode='read_only')
        refused = self.dispatch_from(self.workspace, **base, operation_id='mixed-hosts',
            tasks=[{'id': 'a', 'prompt': 'x', 'host': 'workshop'}, {'id': 'b', 'prompt': 'x', 'host': 'laptop'}])
        self.assertEqual(refused['error'], 'host_conflict', refused)
        launched = self.dispatch_from(self.workspace, **base, operation_id='task-host',
            tasks=[{'id': 'slow-task', 'prompt': 'slow', 'host': 'workshop'}])
        try:
            self.assertNotIn('local', launched)
            self.assertEqual(launched['status'], 'running', launched)
            self.assertTrue(launched['id'].endswith('@workshop'), launched)
        finally:
            self.client('cancel', id=launched['id'], operation_id='stop-task-host')

    def test_cwd_inside_a_registered_project_places_the_request_not_the_chair_directory(self):
        _, _, local = self.writer_fixture()
        chair = self.local_home / 'Repos/chair'
        chair.mkdir()
        subprocess.run(['git', 'init', '-q', str(chair)], check=True)
        launched = self.dispatch_from(chair, host='workshop', adapter='codex', model='fixture',
            prompt='slow', mode='read_only', cwd=str(local), task_id='mapped', operation_id='mapped-cwd')
        try:
            self.assertEqual(launched['status'], 'running', launched)
            self.assertTrue(launched['id'].endswith('@workshop'), launched)
        finally:
            self.client('cancel', id=launched['id'], operation_id='stop-mapped')

    def test_remote_cwd_outside_every_registered_project_is_refused_naming_path_and_host(self):
        self.writer_fixture()
        outside = self.local_home / 'Repos/unregistered'
        outside.mkdir()
        subprocess.run(['git', 'init', '-q', str(outside)], check=True)
        refused = self.dispatch_from(self.workspace, host='workshop', adapter='codex', model='fixture',
            prompt='x', mode='read_only', cwd=str(outside), operation_id='outside-project')
        self.assertEqual(refused['error'], 'invalid_project_path', refused)
        self.assertIn(str(outside), refused['fix'])
        self.assertIn('workshop', refused['fix'])

    def test_project_registered_locally_but_missing_on_the_peer_is_refused_naming_the_host(self):
        git, _, _ = self.writer_fixture()
        other = self.local_home / 'Repos/localonly'
        other.mkdir()
        subprocess.run(['git', 'init', '-q', str(other)], check=True)
        git(other, 'commit', '-q', '--allow-empty', '-m', 'base')
        path = self.local_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(path.read_text())
        config['projects']['Repos/localonly'] = {}
        path.write_text(json.dumps(config))
        refused = self.dispatch_from(self.workspace, host='workshop', adapter='codex', model='fixture',
            prompt='x', mode='read_only', cwd=str(other), operation_id='missing-on-peer')
        self.assertEqual(refused['status'], 'rejected', refused)
        self.assertIn('workshop', json.dumps(refused))
        self.assertIn('Repos/localonly', json.dumps(refused))

    def test_unplaceable_project_error_names_the_checked_path_and_host(self):
        self.writer_fixture()
        chair = self.local_home / 'Repos/broken'
        chair.mkdir()
        (chair / '.git').write_text('gitdir: ' + str(chair / 'missing') + '\n')
        refused = self.dispatch_from(chair, host='workshop', adapter='codex', model='fixture',
            prompt='x', operation_id='unplaceable')
        self.assertEqual(refused['error'], 'invalid_project_path', refused)
        self.assertIn(str(chair), refused['fix'])
        self.assertIn(f'checked {chair} on laptop', refused['fix'])

    def test_reader_cleanup_accepts_case_alias_registration_and_terminal_cwd(self):
        import fabric_hosts as hosts
        git, base, _ = self.writer_fixture()
        git(self.peer_workspace, 'fetch', str(self.workspace), 'HEAD')
        target = self.peer_workspace / '.worktrees/read-case'
        git(self.peer_workspace, 'worktree', 'add', '--detach', str(target), base)
        alias_root = self.peer_home / 'repos'
        if not alias_root.exists():
            alias_root.symlink_to(self.peer_home / 'Repos', target_is_directory=True)
        alias = alias_root / 'project/.worktrees/read-case'
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            code = hosts.lane_owner().code()
            record = code.state_file('reader-contexts', 'case-reader')
            code.write_json(record, [dict(project_path='Repos/project',
                worktree_path=str(alias), base_revision=base)])
            self.assertEqual(code.remove_readers('case-reader', terminal_cwd=str(target)), [])
            self.assertFalse(target.exists())
            self.assertFalse(record.exists())

    def test_remote_prompt_file_in_scratch_is_sent_as_bounded_text(self):
        import fabric_hosts as hosts
        with tempfile.TemporaryDirectory() as directory:
            brief = Path(directory) / 'brief.md'
            brief.write_text('scratch review brief')
            with patch.dict(os.environ, self.env):
                value = hosts.lane_owner().rewrite_paths({'prompt_file': str(brief)}, cwd=str(self.workspace))
                self.assertEqual(value, {'prompt': 'scratch review brief'})
                brief.write_bytes(b'x' * (hosts.MAX_REQUEST_BYTES + 1))
                with self.assertRaises(hosts.HostError) as error:
                    hosts.lane_owner().rewrite_paths({'prompt_file': str(brief)}, cwd=str(self.workspace))
                self.assertEqual(error.exception.code, 'request_too_large')

    def test_invalid_config_lanes_keeps_local_rows_and_visible_note(self):
        (self.local_home / '.agents/.agent-fabric/hosts.json').write_text('{bad')
        result = self.lanes()
        self.assertTrue(result['runs'])
        self.assertIn('invalid_config', result['digest'])
        self.assertEqual(result['hosts'][0]['error']['code'], 'invalid_config')

    def test_empty_peers_lanes_stay_local_and_never_reach_ssh(self):
        (self.local_home / '.agents/.agent-fabric/hosts.json').write_text(
            json.dumps({'schema_version': 1, 'local_host': 'laptop', 'peers': {}}))
        marker = self.root / 'ssh-called'
        self.ssh.write_text('#!/bin/sh\necho called > ' + str(marker) + '\nexit 99\n')
        view = self.lanes()
        # No peers: the plain local view, without a federated host table.
        self.assertEqual(view['schema'], 'fabric.runs.v1')
        self.assertEqual([row['run_id'] for row in view['runs']], ['mcp-shared'])
        self.assertNotIn('hosts', view)
        self.assertFalse(marker.exists())

    def test_writer_transports_committed_base_and_fetches_verified_head_without_github(self):
        git, base, local = self.writer_fixture()
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='writer-once')
        self.assertEqual(launched['status'], 'running', launched)
        remote = self.peer_workspace / '.worktrees/feat-writer'
        self.assertEqual(git(remote, 'rev-parse', 'HEAD'), base)
        self.assertEqual((remote / 'code.txt').read_text(), 'committed base\n')
        self.assertFalse(launched['code_transport']['uncommitted_changes_transported'])
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.lanes('writer@workshop')['runs'][0]['state'] == 'terminal':
                break
            time.sleep(.05)
        (remote / 'code.txt').write_text('remote implementation\n')
        git(remote, 'add', 'code.txt')
        git(remote, 'commit', '-qm', 'fixture implementation')
        head = git(remote, 'rev-parse', 'HEAD')
        fetched = self.client('fetch', id='writer-once@workshop')
        self.assertEqual(fetched['status'], 'ok', fetched)
        self.assertEqual(fetched['verification']['base_revision'], base)
        self.assertEqual(fetched['verification']['head_revision'], head)
        self.assertEqual(fetched['verification']['host'], 'workshop')
        self.assertEqual(git(self.workspace, 'rev-parse', fetched['ref']), head)
        self.assertEqual((local / 'code.txt').read_text(), 'uncommitted chair edit\n')
        replay = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='writer-once')
        self.assertEqual(replay['id'], launched['id'])
        cli = subprocess.run([str(ROOT / 'scripts/provenant'), 'fabric', 'fetch', 'writer-once@workshop'],
            cwd=self.workspace, env=self.env, capture_output=True, text=True, timeout=30)
        self.assertEqual(cli.returncode, 0, cli.stdout + cli.stderr)
        self.assertEqual(json.loads(cli.stdout)['verification']['head_revision'], head)
        for seat in ['codex', 'agy']:
            fetched_mcp = self.mcp('fabric_fetch', {'id': 'writer@workshop'}, seat)
            self.assertEqual(fetched_mcp['verification']['head_revision'], head)

    def test_successful_writer_retry_cannot_change_host_mode_or_fall_back(self):
        _, _, local = self.writer_fixture()
        request = dict(adapter='codex', model='fixture', prompt='fixture only',
            mode='worktree_write', worktree=str(local), task_id='writer', operation_id='bound-writer')
        self.offline.touch()
        unsent = self.client('dispatch', **{**request, 'operation_id': 'unsent-writer'})
        self.assertTrue(unsent.get('local'), unsent)
        self.offline.unlink()
        refused_retry = self.client('dispatch', **{**request, 'operation_id': 'unsent-writer'})
        self.assertEqual(refused_retry['error'], 'operation_conflict', refused_retry)
        launched = self.client('dispatch', **request)
        self.assertEqual(launched['status'], 'running', launched)
        try:
            config_path = self.local_home / '.agents/.agent-fabric/hosts.json'
            config = json.loads(config_path.read_text())
            config['peers']['other'] = {'ssh_destination': 'other'}
            config_path.write_text(json.dumps(config))
            for changed in [{**request, 'host': 'other'}, {**request, 'host': 'laptop'},
                            {k: ('read_only' if k == 'mode' else v) for k, v in request.items() if k != 'worktree'}]:
                with self.subTest(changed=changed):
                    rejected = self.client('dispatch', **changed)
                    self.assertEqual(rejected.get('error'), 'operation_conflict', rejected)
            self.offline.touch()
            retry = self.client('dispatch', **request)
            self.assertEqual(retry.get('state'), 'launch_unknown', retry)
            self.assertFalse(retry.get('local'), retry)
        finally:
            self.offline.unlink(missing_ok=True)
            self.client('cancel', id='writer@workshop', operation_id='cleanup-bound-writer')

    def test_unsent_explicit_writer_binding_cannot_fall_back_on_retry(self):
        _, _, local = self.writer_fixture()
        request = dict(adapter='codex', model='fixture', prompt='fixture only',
            mode='worktree_write', worktree=str(local), task_id='writer', operation_id='unsent-explicit')
        self.offline.touch()
        initial = self.client('dispatch', **request, host='workshop')
        self.assertEqual(initial['status'], 'rejected', initial)
        for placement in ({}, {'host': 'laptop'}):
            with self.subTest(placement=placement):
                retry = self.client('dispatch', **request, **placement)
                self.assertFalse(retry.get('local'), retry)
        self.offline.unlink()
        launched = self.client('dispatch', **request, host='workshop')
        self.assertEqual(launched['status'], 'running', launched)
        self.wait_writer(launched['id'])

    def test_frozen_writer_branch_conflict_requires_a_new_operation_id(self):
        import fabric_hosts as hosts
        git, _, local = self.writer_fixture()
        identifier = 'frozen-branch-conflict'
        request = dict(host='workshop', adapter='codex', model='fixture', prompt='fixture only',
            mode='worktree_write', worktree=str(local), task_id='writer', operation_id=identifier)
        with patch.dict(os.environ, self.env):
            code = hosts.lane_owner().code()
            client = hosts.PeerClient('workshop', code.f.config.peers['workshop'])
            code.outgoing('Repos/project', request, str(self.workspace), identifier, 'workshop', client)
        git(local, 'branch', '-m', 'feat/renamed')
        renamed = local.with_name('feat-renamed')
        git(self.workspace, 'worktree', 'move', str(local), str(renamed))
        rejected = self.client('dispatch', **{**request, 'worktree': str(renamed)})
        self.assertEqual(rejected['error'], 'operation_conflict', rejected)
        self.assertIn('new operation ID', rejected['fix'])

    def test_unsent_batch_retires_frozen_inputs_before_changing_peer(self):
        self.set_peer_response_deadline(30)
        git, base, local = self.writer_fixture()
        other = self.workspace / '.worktrees/feat-other'
        git(self.workspace, 'worktree', 'add', '-b', 'feat/mismatched', str(other), base)
        request = dict(host='workshop', adapter='codex', model='fixture', mode='worktree_write',
            operation_id='unsent-batch', tasks=[
                {'id': 'writer-a', 'prompt': 'fixture only', 'worktree': str(local)},
                {'id': 'writer-b', 'prompt': 'fixture only', 'worktree': str(other)}])
        grafts = self.workspace / '.git/info/grafts'
        grafts.write_text(base + '\n')
        unsupported = self.client('dispatch', **request)
        self.assertEqual(unsupported['error'], 'code_transport_unsupported', unsupported)
        self.assertIn('same operation ID', unsupported['fix'])
        grafts.unlink()
        refused = self.client('dispatch', **request)
        self.assertEqual(refused['error'], 'worktree_invalid', refused)
        self.assertEqual(list(self.local_home.rglob('unsent-batch-*/outgoing.json')), [])
        self.assertEqual(git(self.workspace, 'for-each-ref', '--format=%(refname)',
            'refs/provenant/hosts/workshop/unsent-batch-'), '')
        git(other, 'branch', '-m', 'feat/other')
        # Repoint the loopback peer under a new host identity.
        for home in (self.local_home, self.peer_home):
            path = home / '.agents/.agent-fabric/hosts.json'
            config = json.loads(path.read_text())
            if home == self.local_home:
                config['peers']['other'] = {'ssh_destination': 'other', 'response_deadline': 30}
            else:
                config['local_host'] = 'other'
            path.write_text(json.dumps(config))
        launched = self.client('dispatch', **{**request, 'host': 'other'})
        self.assertEqual(launched['status'], 'running', launched)
        self.wait_writer('writer-a@other')
        self.wait_writer('writer-b@other')

    def test_failed_peer_prepare_removes_worktrees_created_before_later_failure(self):
        self.set_peer_response_deadline(30)
        git, base, local = self.writer_fixture()
        other = self.workspace / '.worktrees/feat-other'
        git(self.workspace, 'worktree', 'add', '-b', 'feat/other', str(other), base)
        # The second writer cannot create its branch on the peer.
        git(self.peer_workspace, 'fetch', str(self.workspace), 'HEAD')
        git(self.peer_workspace, 'branch', 'feat/other', 'FETCH_HEAD')
        request = dict(host='workshop', adapter='codex', model='fixture', mode='worktree_write',
            tasks=[{'id': 'writer-a', 'prompt': 'fixture only', 'worktree': str(local)},
                   {'id': 'writer-b', 'prompt': 'fixture only', 'worktree': str(other)}])
        refused = self.client('dispatch', **request, operation_id='failed-prepare')
        self.assertEqual(refused['error'], 'worktree_invalid', refused)
        self.assertFalse((self.peer_workspace / '.worktrees/feat-writer').exists())
        self.assertEqual(git(self.peer_workspace, 'for-each-ref', '--format=%(refname)',
            'refs/heads/feat/writer'), '')
        self.assertEqual(git(self.peer_workspace, 'rev-parse', 'feat/other'), base)
        git(self.peer_workspace, 'update-ref', '-d', 'refs/heads/feat/other', base)
        launched = self.client('dispatch', **request, operation_id='after-failed-prepare')
        self.assertEqual(launched['status'], 'running', launched)
        self.wait_writer('writer-a@workshop')
        self.wait_writer('writer-b@workshop')

    def interrupted_checkout(self, git, base, branch=None):
        """A Git worktree stopped while its initializing lock was held."""
        name = (branch or 'read-interrupted').replace('/', '-')
        target = self.peer_workspace / '.worktrees' / name
        command = ['worktree', 'add', '--no-checkout', *(['-b', branch, str(target), base] if branch
            else ['--detach', str(target), base])]
        git(self.peer_workspace, *command)
        metadata = Path(git(target, 'rev-parse', '--absolute-git-dir'))
        (metadata / 'locked').write_text('initializing')
        return target, metadata

    def settle_with(self, directory, context, identifier):
        import fabric_hosts as hosts
        patcher = patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
            'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')})
        patcher.start()
        self.addCleanup(patcher.stop)  # Later settlements in the test run as the peer too.
        code = hosts.lane_owner().code()
        code.write_json(code.state_file(directory, identifier), [context])
        with code.f.database() as db:
            db.execute('INSERT OR IGNORE INTO operations VALUES (?,?,NULL)', (identifier,
                json.dumps({'project': 'Repos/project', 'verb': 'dispatch', 'digest': 'fixture'})))
        return code, code.f.settle(identifier)

    def test_initializing_checkouts_are_preserved_even_with_intent_and_proof(self):
        """A genuine initializing lock may be a live competitor's git worktree add."""
        git, base, _ = self.writer_fixture()
        git(self.peer_workspace, 'fetch', str(self.workspace), 'HEAD')
        target, metadata = self.interrupted_checkout(git, base, 'feat/initializing')
        context = dict(project_path='Repos/project', worktree_path=str(target), branch='feat/initializing',
            base_revision=base, host='workshop', operation_id='init-writer', created_worktree=True,
            created_branch=True, creation_nonce='nonce-writer')
        (target / 'code.txt').write_text('committed base\n')
        (target / 'code.txt').chmod(0o654)  # Git reads 100755 -> 100644; the gate no longer matters.
        code, result = self.settle_with('writer-contexts', context, 'init-writer')
        self.assertEqual(result['cleanup_pending'], [str(target)])
        code.prove_creation(target, context)
        self.assertEqual(code.f.settle('init-writer')['cleanup_pending'], [str(target)])
        self.assertTrue(target.exists())
        self.assertTrue(metadata.exists())
        reader = dict(project_path='Repos/project', worktree_path=str(target), worktree=target.name,
            read_only=True, base_revision=base, host='workshop', operation_id='init-reader',
            created_worktree=True, creation_nonce='nonce-writer')
        code.write_json(code.state_file('reader-contexts', 'init-reader'), [reader])
        with code.f.database() as db:
            db.execute('INSERT INTO operations VALUES (?,?,NULL)', ('init-reader',
                json.dumps({'project': 'Repos/project', 'verb': 'dispatch', 'digest': 'fixture'})))
        self.assertEqual(code.f.settle('init-reader')['cleanup_pending'], [str(target)])
        self.assertTrue(target.exists())

    def test_cleanup_never_forces_removal_and_keeps_content_staged_after_validation(self):
        git, base, _ = self.writer_fixture()
        git(self.peer_workspace, 'fetch', str(self.workspace), 'HEAD')
        target = self.peer_workspace / '.worktrees/feat-raced'
        git(self.peer_workspace, 'worktree', 'add', '-b', 'feat/raced', str(target), base)
        context = dict(project_path='Repos/project', worktree_path=str(target), branch='feat/raced',
            base_revision=base, host='workshop', operation_id='raced', created_worktree=True,
            created_branch=True, creation_nonce='nonce-raced')
        import fabric_hosts as hosts
        patcher = patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
            'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')})
        patcher.start()
        self.addCleanup(patcher.stop)
        code = hosts.lane_owner().code()
        code.prove_creation(target, context)
        commands = []
        real = type(code).git

        def stage_after_validation(owner, directory, *args, **kwargs):
            commands.append(args)
            if args[:2] == ('worktree', 'remove'):
                # Native Git, outside the advisory lease, stages unique recovery content.
                (target / 'code.txt').write_text('unique staged recovery content\n')
                git(target, 'add', 'code.txt')
            return real(owner, directory, *args, **kwargs)

        with patch.object(type(code), 'git', stage_after_validation):
            code.write_json(code.state_file('writer-contexts', 'raced'), [context])
            with code.f.database() as db:
                db.execute('INSERT INTO operations VALUES (?,?,NULL)', ('raced',
                    json.dumps({'project': 'Repos/project', 'verb': 'dispatch', 'digest': 'fixture'})))
            result = code.f.settle('raced')
        removals = [args for args in commands if args[:2] == ('worktree', 'remove')]
        self.assertTrue(removals)
        self.assertFalse(any(flag in args for args in commands for flag in ('--force', '-f')))
        self.assertEqual(result['cleanup_pending'], [str(target)])
        self.assertEqual((target / 'code.txt').read_text(), 'unique staged recovery content\n')
        self.assertEqual(git(target, 'diff', '--cached', '--name-only'), 'code.txt')

    def test_proof_creation_never_follows_or_overwrites_a_planted_file(self):
        git, base, _ = self.writer_fixture()
        git(self.peer_workspace, 'fetch', str(self.workspace), 'HEAD')
        target = self.peer_workspace / '.worktrees/read-planted'
        git(self.peer_workspace, 'worktree', 'add', '--detach', str(target), base)
        metadata = Path(git(target, 'rev-parse', '--absolute-git-dir'))
        victim = self.root / 'victim.txt'
        victim.write_text('keep me\n')
        (metadata / 'provenant-creation').symlink_to(victim)
        import fabric_hosts as hosts
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            code = hosts.lane_owner().code()
            with self.assertRaises(OSError):
                code.prove_creation(target, dict(creation_nonce='nonce'))
        self.assertEqual(victim.read_text(), 'keep me\n')

    def test_reader_continuation_publishes_proof_and_failed_prelaunch_is_cleaned(self):
        import fabric_hosts as hosts
        git, base, _ = self.writer_fixture()
        git(self.peer_workspace, 'fetch', str(self.workspace), 'HEAD')
        previous = self.peer_workspace / '.worktrees/read-previous-0'
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            code = hosts.lane_owner().code()
            prior = dict(project_path='Repos/project', worktree_path=str(previous), worktree=previous.name,
                read_only=True, base_revision=base, host='workshop', operation_id='previous', index=0,
                branch='read/previous-0', task_id=None)
            code.write_json(code.state_file('reader-inputs', 'previous'), [prior])
            code.write_json(code.state_file('operations', 'previous'), {'runId': 'run-1', 'taskId': '*', 'attempt': 1})
            with code.f.database() as db:
                db.execute('INSERT INTO operations VALUES (?,?,NULL)', ('continued',
                    json.dumps({'project': 'Repos/project', 'verb': 'resume', 'digest': 'fixture'})))
            contexts = code.reader_continuation('Repos/project', {'run_id': 'run-1', 'cwd': str(previous)}, 'continued')
            target = Path(contexts[0]['worktree_path'])
            self.assertTrue(code.proven_creation(target, contexts[0]))
            # The continuation never launched: settlement now removes the proven checkout.
            self.assertEqual(code.release('continued'), [])
            self.assertFalse(target.exists())
            self.assertFalse(code.state_file('reader-contexts', 'continued').exists())

    def test_post_creation_failure_keeps_the_ownership_record(self):
        import fabric_hosts as hosts
        git, base, local = self.writer_fixture()
        self.writer_fixture_state = (git, base, local)
        request = self.reader_input(git, 'reader-post')
        target = self.peer_workspace / '.worktrees/read-reader-post-0'
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            code = hosts.lane_owner().code()
            with code.f.database() as db:
                db.execute('INSERT INTO operations VALUES (?,?,NULL)', ('reader-post',
                    json.dumps({'project': 'Repos/project', 'verb': 'dispatch', 'digest': 'fixture'})))

            def created_then_failed(args):
                git(self.peer_workspace, 'worktree', 'add', '--detach', str(target), base)
                raise code.w.PostCreationError('cannot determine new worktree branch identity')

            with code.f.operation_lock('reader-post'), \
                    patch.object(code.w, 'create', side_effect=created_then_failed), self.assertRaises(hosts.HostError):
                code.prepare('Repos/project', request, 'reader-post')
            self.assertTrue(code.state_file('reader-contexts', 'reader-post').exists())
            self.assertEqual(code.release('reader-post'), [str(target)])  # Unproven, so preserved and reported.
            self.assertTrue(target.exists())

    def reader_input(self, git, identifier, cwd=None):
        """Upload a frozen reader input to the peer, returning the request and the peer environment."""
        import fabric_hosts as hosts
        _, _, local = self.writer_fixture_state
        with patch.dict(os.environ, self.env):
            code = hosts.lane_owner().code()
            client = hosts.PeerClient('workshop', code.f.config.peers['workshop'])
            request = code.outgoing('Repos/project', dict(adapter='codex', model='fixture', mode='read_only',
                prompt='fixture only', cwd=str(cwd or local)), str(self.workspace), identifier, 'workshop', client)
            code.send(client, 'Repos/project', request['code_transport'])
        return request

    def test_refused_reader_creation_never_lets_cleanup_remove_a_competitors_checkout(self):
        import fabric_hosts as hosts
        git, base, local = self.writer_fixture()
        self.writer_fixture_state = (git, base, local)
        request = self.reader_input(git, 'reader-collision')
        target = self.peer_workspace / '.worktrees/read-reader-collision-0'
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            code = hosts.lane_owner().code()
            with code.f.database() as db:
                db.execute('INSERT INTO operations VALUES (?,?,NULL)', ('reader-collision',
                    json.dumps({'project': 'Repos/project', 'verb': 'dispatch', 'digest': 'fixture'})))

            def competitor(args):
                git(self.peer_workspace, 'worktree', 'add', '--detach', str(target), base)
                (target / 'unrelated.txt').write_text('belongs to the competitor\n')
                raise code.w.PolicyError('worktree target already exists')

            with code.f.operation_lock('reader-collision'), \
                    patch.object(code.w, 'create', side_effect=competitor), self.assertRaises(hosts.HostError) as raised:
                code.prepare('Repos/project', request, 'reader-collision')
            self.assertEqual(raised.exception.code, 'worktree_invalid')
            self.assertEqual(code.release('reader-collision'), [])
            self.assertEqual((target / 'unrelated.txt').read_text(), 'belongs to the competitor\n')
            self.assertFalse(code.state_file('reader-contexts', 'reader-collision').exists())
            self.assertEqual(list((self.peer_home / '.agents/.agent-fabric/code').glob('*/reader-collision-0/input.bundle')), [])

    def test_creation_intent_without_proof_never_removes_a_competitors_checkout(self):
        git, base, _ = self.writer_fixture()
        git(self.peer_workspace, 'fetch', str(self.workspace), 'HEAD')
        target = self.peer_workspace / '.worktrees/read-intent-only-0'
        git(self.peer_workspace, 'worktree', 'add', '--detach', str(target), base)
        context = dict(project_path='Repos/project', worktree_path=str(target), worktree=target.name,
            read_only=True, base_revision=base, host='workshop', operation_id='intent-only',
            created_worktree=True, creation_nonce='abc123')
        code, result = self.settle_with('reader-contexts', context, 'intent-only')
        self.assertEqual(result['cleanup_pending'], [str(target)])
        self.assertTrue(target.exists())
        code.prove_creation(target, context)
        self.assertNotIn('cleanup_pending', code.f.settle('intent-only'))
        self.assertFalse(target.exists())

    def test_reader_subdirectory_through_a_committed_symlink_is_refused(self):
        import fabric_hosts as hosts
        git, base, local = self.writer_fixture()
        (local / 'real/pkg').mkdir(parents=True)
        (local / 'real/pkg/input.txt').write_text('package input\n')
        (local / 'bridge').symlink_to('real')
        git(local, 'add', '.')
        git(local, 'commit', '-qm', 'bridge')
        self.writer_fixture_state = (git, git(local, 'rev-parse', 'HEAD'), local)
        request = self.reader_input(git, 'reader-bridge', cwd=local / 'real/pkg')
        binding = request['code_transport'][0]
        self.assertEqual(binding['subdir'], 'real/pkg')
        binding['subdir'] = 'bridge/pkg'
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            code = hosts.lane_owner().code()
            incoming = code.directory('Repos/project', 'reader-bridge-0', receiving=True) / 'incoming.json'
            meta = json.loads(incoming.read_text())
            meta['subdir'] = 'bridge/pkg'
            incoming.write_text(json.dumps(meta))
            with code.f.operation_lock('reader-bridge'), self.assertRaises(hosts.HostError) as raised:
                code.prepare('Repos/project', request, 'reader-bridge')
            self.assertEqual(raised.exception.code, 'worktree_invalid')

    def test_release_matches_the_exact_operation_and_task_index(self):
        import fabric_hosts as hosts
        git, base, local = self.writer_fixture()
        self.writer_fixture_state = (git, base, local)
        self.reader_input(git, 'prefix-sibling')
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            code = hosts.lane_owner().code()
            transfer = code.directory('Repos/project', 'prefix-sibling-0', receiving=True)
            before = sorted(item.name for item in transfer.iterdir())
            self.assertTrue(before)
            code.release('prefix')
            self.assertEqual(sorted(item.name for item in transfer.iterdir()), before)

    def test_reader_keeps_the_requested_subdirectory_through_dispatch_and_resume(self):
        git, base, local = self.writer_fixture()
        (local / 'pkg').mkdir()
        (local / 'pkg/input.txt').write_text('package input\n')
        git(local, 'add', 'pkg/input.txt')
        git(local, 'commit', '-qm', 'package input')
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='read_only', cwd=str(local / 'pkg'), task_id='reader-pkg', operation_id='reader-pkg')
        self.assertEqual(launched['status'], 'running', launched)
        reader = launched['code_transport']['readers'][0]
        root = Path(reader['worktree_path'])
        self.assertEqual(reader['subdir'], 'pkg')
        self.assertTrue((root / 'pkg/input.txt').exists())
        row = self.client('status', ids=[launched['id']], detail='full')['runs'][0]
        self.assertEqual(row['cwd'], str(root / 'pkg'))
        self.wait_writer(launched['id'])
        self.assertFalse(root.exists())
        resumed = self.client('resume', resume=launched['id'], prompt='slow', operation_id='reader-pkg-next')
        self.assertNotEqual(resumed.get('status'), 'rejected', resumed)
        try:
            again = Path(resumed['code_transport']['readers'][0]['worktree_path'])
            row = self.client('status', ids=[resumed['id']], detail='full')['runs'][0]
            self.assertEqual(row['cwd'], str(again / 'pkg'))
            self.assertTrue((again / 'pkg/input.txt').exists())
        finally:
            self.client('cancel', id=launched['id'], operation_id='stop-reader-pkg')
        self.wait_writer(launched['id'])

    def test_unexpected_preparation_error_releases_input_and_records_a_rejection(self):
        import fabric_hosts as hosts
        self.writer_fixture()
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            owner = hosts.lane_owner()
            code = owner.code()
            transfer = code.directory('Repos/project', 'prepare-boom-0', receiving=True)
            (transfer / 'input.bundle').write_bytes(b'unconsumed')
            (transfer / 'chunks').mkdir()
            with patch.object(owner, 'code', return_value=code), \
                    patch.object(code, 'prepare', side_effect=ValueError('unexpected')):
                result = owner.peer_call('dispatch', {'project_path': 'Repos/project', 'input': {
                    'operation_id': 'prepare-boom', 'prompt': 'review', 'mode': 'read_only',
                    'code_transport': [{'index': 0}]}})
            self.assertEqual(result['status'], 'rejected', result)
            self.assertFalse((transfer / 'input.bundle').exists())
            self.assertFalse((transfer / 'chunks').exists())
            with owner.database() as db:
                self.assertIsNotNone(db.execute('SELECT result FROM operations WHERE id=?', ('prepare-boom',)).fetchone()[0])

    def test_failed_prepare_settles_after_project_disappears_from_peer_config(self):
        import fabric_hosts as hosts
        git, base, local = self.writer_fixture()
        other = self.workspace / '.worktrees/feat-other'
        git(self.workspace, 'worktree', 'add', '-b', 'feat/other', str(other), base)
        git(self.peer_workspace, 'fetch', str(self.workspace), 'HEAD')
        git(self.peer_workspace, 'branch', 'feat/other', 'FETCH_HEAD')
        identifier = 'missing-project-prepare'
        with patch.dict(os.environ, self.env):
            code = hosts.lane_owner().code()
            client = hosts.PeerClient('workshop', code.f.config.peers['workshop'])
            request = code.outgoing('Repos/project', dict(adapter='codex', model='fixture',
                mode='worktree_write', tasks=[
                    {'id': 'writer-a', 'prompt': 'fixture only', 'worktree': str(local)},
                    {'id': 'writer-b', 'prompt': 'fixture only', 'worktree': str(other)}]),
                str(self.workspace), identifier, 'workshop', client)
            code.send(client, 'Repos/project', request['code_transport'])
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            code = hosts.lane_owner().code()
            with code.f.database() as db:
                db.execute('INSERT INTO operations VALUES (?,?,NULL)', (identifier,
                    json.dumps({'project': 'Repos/project', 'verb': 'dispatch', 'digest': 'fixture'})))
            with code.f.operation_lock(identifier), self.assertRaises(hosts.HostError) as raised:
                code.prepare('Repos/project', request, identifier)
            self.assertEqual(raised.exception.code, 'worktree_invalid')
        config_path = self.peer_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(config_path.read_text())
        config.pop('projects')
        config_path.write_text(json.dumps(config))
        response = self.peer('operation', operation_id=identifier)
        self.assertTrue(response['ok'], response)
        result = response['result']
        self.assertEqual(result['error'], 'launch_not_started', result)
        remote = self.peer_workspace / '.worktrees/feat-writer'
        self.assertEqual(result['cleanup_pending'], [str(remote)])
        self.assertEqual(git(remote, 'rev-parse', 'HEAD'), base)
        record = self.peer_home / '.agents/.agent-fabric/writer-contexts' / (identifier + '.json')
        self.assertEqual(json.loads(record.read_text())[0]['worktree_path'], str(remote))
        self.assertEqual(self.peer('operation', operation_id=identifier)['result'], result)

    def test_unstarted_cleanup_records_any_failure_and_clears_it_after_a_clean_retry(self):
        import fabric_hosts as hosts
        import worktree
        identifier = 'cleanup-policy-error'

        class Transport:
            def __init__(self, outcome):
                self.outcome = outcome

            def release(self, identifier, held):
                if isinstance(self.outcome, Exception):
                    raise self.outcome
                return self.outcome

        with patch.dict(os.environ, self.env):
            owner = hosts.lane_owner()
            with owner.database() as db:
                db.execute('INSERT INTO operations VALUES (?,?,NULL)', (identifier, json.dumps({})))
            rejected = {'status': 'rejected', 'error': 'launch_not_started'}
            failed = owner.cleanup_unstarted(identifier, dict(rejected),
                transport=Transport(worktree.PolicyError('worktree is not owned')))
            self.assertEqual(failed['cleanup_error'], 'worktree is not owned')
            cleaned = owner.cleanup_unstarted(identifier, failed, transport=Transport([]))
            self.assertEqual(cleaned, rejected)
            with owner.database() as db:
                stored = db.execute('SELECT result FROM operations WHERE id=?', (identifier,)).fetchone()[0]
            self.assertEqual(json.loads(stored), rejected)

    def test_peer_transport_rejection_requires_a_new_operation_after_repair(self):
        import base64
        import hashlib
        _, base, local = self.writer_fixture()
        data = b'not a git bundle\n'
        meta = dict(base_revision=base, branch='feat/writer', worktree='feat-writer',
            size=len(data), sha256=hashlib.sha256(data).hexdigest())
        self.assertTrue(self.peer('git-upload', operation_id='bad-prepare-0', metadata=meta,
            offset=0, data=base64.b64encode(data).decode())['ok'])
        request = dict(operation_id='bad-prepare', adapter='codex', model='fixture', prompt='fixture only',
            mode='worktree_write', task_id='writer', code_transport=[dict(index=0, transfer_id='bad-prepare-0', **meta)])
        rejected = self.peer('dispatch', **request)['result']
        self.assertEqual(rejected['error'], 'code_transport_failed', rejected)
        self.assertIn('new operation ID', rejected['fix'])
        self.assertEqual(self.peer('dispatch', **request)['result'], rejected)
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='repaired-prepare')
        self.assertEqual(launched['status'], 'running', launched)
        self.wait_writer(launched['id'])

    def test_transport_restores_normal_git_configuration_after_success_and_failure(self):
        import fabric_hosts as hosts
        git, _, _ = self.writer_fixture()
        git(self.workspace, 'config', 'core.fsmonitor', 'true')
        with patch.dict(os.environ, self.env):
            code = hosts.lane_owner().code()
            code.repo('Repos/project')
            self.assertEqual(code.w.git(self.workspace, 'config', 'core.fsmonitor').stdout.strip(), 'true')
            with self.assertRaises(hosts.HostError):
                code.repo('Repos/missing')
            self.assertEqual(code.w.git(self.workspace, 'config', 'core.fsmonitor').stdout.strip(), 'true')

    def test_dead_creator_is_removed_with_or_without_a_published_claim(self):
        import fabric_hosts as hosts
        git, base, local = self.writer_fixture()
        for state in ('claimed', 'unclaimed', 'moved'):
            identifier = 'dead-created-' + state
            with self.subTest(state=state):
                with patch.dict(os.environ, self.env):
                    code = hosts.lane_owner().code()
                    client = hosts.PeerClient('workshop', code.f.config.peers['workshop'])
                    request = code.outgoing('Repos/project', dict(adapter='codex', model='fixture',
                        prompt='fixture only', mode='worktree_write', worktree=str(local), task_id='writer'),
                        str(self.workspace), identifier, 'workshop', client)
                    code.send(client, 'Repos/project', request['code_transport'])
                with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                        'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
                    code = hosts.lane_owner().code()
                    with code.f.database() as db:
                        db.execute('INSERT INTO operations VALUES (?,?,NULL)', (identifier,
                            json.dumps({'project': 'Repos/project', 'verb': 'dispatch', 'digest': 'fixture'})))
                    with code.f.operation_lock(identifier):
                        context = code.prepare('Repos/project', request, identifier)[0]
                        if state == 'unclaimed':
                            code.context_owner(Path(context['worktree_path'])).unlink()
                moved = self.peer_workspace / '.worktrees/moved-writer'
                if state == 'moved':
                    git(self.peer_workspace, 'worktree', 'move', context['worktree_path'], str(moved))
                result = self.peer('operation', operation_id=identifier)['result']
                self.assertEqual(result['error'], 'launch_not_started', result)
                self.assertFalse((self.peer_workspace / '.worktrees/feat-writer').exists())
                if state == 'moved':
                    self.assertEqual(git(moved, 'rev-parse', 'HEAD'), base)
                    self.assertEqual(git(self.peer_workspace, 'rev-parse', 'feat/writer'), base)
                else:
                    self.assertEqual(git(self.peer_workspace, 'for-each-ref', '--format=%(refname)',
                        'refs/heads/feat/writer'), '')
                self.assertEqual(self.peer('operation', operation_id=identifier)['result'], result)

    def test_creation_record_requires_a_durable_parent_directory(self):
        import fabric_hosts as hosts
        fsync = os.fsync
        with patch.dict(os.environ, self.env):
            code = hosts.lane_owner().code()
            parent = code.f.root.stat()
            def fail_parent_sync(fd):
                inode = os.fstat(fd)
                if (inode.st_dev, inode.st_ino) == (parent.st_dev, parent.st_ino):
                    raise OSError('state directory sync failed')
                return fsync(fd)
            with patch.object(os, 'fsync', side_effect=fail_parent_sync), self.assertRaises(OSError):
                code.write_json(code.state_file('writer-contexts', 'unsynced-parent'),
                    [{'created_worktree': True, 'created_branch': True}])
            self.assertFalse((code.f.root / 'writer-contexts/unsynced-parent.json').exists())

    def test_creation_record_sync_failure_prevents_worktree_creation(self):
        import fabric_hosts as hosts
        _, _, local = self.writer_fixture()
        identifier = 'unsynced-creation'
        with patch.dict(os.environ, self.env):
            code = hosts.lane_owner().code()
            client = hosts.PeerClient('workshop', code.f.config.peers['workshop'])
            request = code.outgoing('Repos/project', dict(adapter='codex', model='fixture',
                prompt='fixture only', mode='worktree_write', worktree=str(local), task_id='writer'),
                str(self.workspace), identifier, 'workshop', client)
            code.send(client, 'Repos/project', request['code_transport'])
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            code = hosts.lane_owner().code()
            binding = request['code_transport'][0]
            code.assemble(code.directory('Repos/project', binding['transfer_id'], receiving=True), binding)
            fsync = os.fsync
            def fail_creation_sync(fd):
                record = code.f.root / 'writer-contexts'
                inode = os.fstat(fd)
                if record.exists() and any((path.stat().st_dev, path.stat().st_ino) == (inode.st_dev, inode.st_ino)
                        for path in (record, *record.iterdir())):
                    raise OSError('creation record sync failed')
                return fsync(fd)
            with patch.object(os, 'fsync', side_effect=fail_creation_sync), self.assertRaises(OSError):
                code.prepare('Repos/project', request, identifier)
            self.assertFalse((self.peer_workspace / '.worktrees/feat-writer').exists())
            self.assertEqual(list((code.f.root / 'writer-contexts').iterdir()), [])

    def test_failed_peer_creation_preserves_a_competing_creators_worktree(self):
        import fabric_hosts as hosts
        git, base, local = self.writer_fixture()
        identifier = 'lost-creation-race'
        with patch.dict(os.environ, self.env):
            federation = hosts.lane_owner()
            code = federation.code()
            client = hosts.PeerClient('workshop', federation.config.peers['workshop'])
            request = code.outgoing('Repos/project', federation.rewrite_paths(dict(adapter='codex',
                model='fixture', prompt='fixture only', mode='worktree_write', worktree=str(local),
                task_id='writer'), cwd=str(self.workspace)), str(self.workspace), identifier, 'workshop', client)
            code.send(client, 'Repos/project', request['code_transport'])
        popen = subprocess.Popen
        competed = False
        def competing_git(command, **kwargs):
            nonlocal competed
            if not competed and command[3:5] == ['worktree', 'add']:
                competed = True
                winner = popen(command, **kwargs)
                _, stderr = winner.communicate()
                self.assertEqual(winner.returncode, 0, stderr)
            return popen(command, **kwargs)
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}), \
                patch.object(subprocess, 'Popen', side_effect=competing_git):
            result = hosts.lane_owner().peer_call('dispatch', {'project_path': 'Repos/project',
                'input': {**request, 'operation_id': identifier}})
        self.assertTrue(competed)
        self.assertEqual(result['error'], 'worktree_invalid', result)
        remote = self.peer_workspace / '.worktrees/feat-writer'
        self.assertTrue(remote.exists())
        self.assertEqual(git(remote, 'rev-parse', 'HEAD'), base)
        self.assertEqual(git(self.peer_workspace, 'rev-parse', 'feat/writer'), base)
        self.assertFalse((self.peer_home / '.agents/.agent-fabric/writer-contexts' / (identifier + '.json')).exists())
    def test_writer_fetch_refuses_active_lease_dirty_context_and_changed_branch(self):
        git, _, local = self.writer_fixture()
        result = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='verify-writer')
        self.assertEqual(result['status'], 'running', result)
        remote = self.peer_workspace / '.worktrees/feat-writer'
        try:
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline and self.lanes('writer@workshop')['runs'][0]['state'] != 'terminal':
                time.sleep(.05)
            # No new commit is rejected by the existing verification owner.
            self.assertEqual(self.client('fetch', id='verify-writer@workshop')['error'], 'writer_verification_failed')
            (remote / 'code.txt').write_text('implementation\n')
            git(remote, 'add', 'code.txt')
            git(remote, 'commit', '-qm', 'implementation')
            (remote / 'untracked.txt').write_text('residue')
            self.assertEqual(self.client('fetch', id='verify-writer@workshop')['error'], 'writer_verification_failed')
            (remote / 'untracked.txt').unlink()
            import fcntl
            gitdir = Path(git(remote, 'rev-parse', '--absolute-git-dir'))
            with (gitdir / 'provenant-dispatch-writer.lock').open('a') as lock:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                self.assertEqual(self.client('fetch', id='verify-writer@workshop')['error'], 'writer_verification_failed')
            git(remote, 'checkout', '-qb', 'feat/rebound')
            self.assertEqual(self.client('fetch', id='verify-writer@workshop')['error'], 'writer_verification_failed')
            git(remote, 'checkout', 'feat/writer')
            self.assertEqual(self.client('fetch', id='verify-writer@workshop')['status'], 'ok')
        finally:
            self.client('cancel', id='writer@workshop', operation_id='cleanup-verify-writer')

    def wait_writer(self, selector):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            rows = self.lanes(selector)['runs']
            if rows and all(row['state'] == 'terminal' and row.get('pgid_alive') is not True for row in rows):
                return rows
            time.sleep(.05)
        self.fail('fixture writer did not terminalise')

    def test_interrupted_multichunk_writer_upload_and_lost_launch_reconcile_once(self):
        git, _, local = self.writer_fixture()
        (local / 'large.bin').write_bytes(os.urandom(100000))
        git(local, 'add', 'large.bin')
        git(local, 'commit', '-qm', 'large committed input')
        request = dict(host='workshop', adapter='codex', model='fixture', prompt='fixture only',
            mode='worktree_write', worktree=str(local), task_id='writer', operation_id='upload-writer')
        (self.root / 'drop-upload').touch()
        launched = self.client('dispatch', **request)
        self.assertEqual(launched['state'], 'launch_unknown', launched)
        self.assertFalse((self.peer_workspace / '.worktrees/feat-writer').exists())
        # While the upload is still unavailable, reads leave reconciliation pending.
        view = self.lanes()
        self.assertTrue(any(row.get('operation_id') == 'upload-writer' and row['state'] == 'launch_unknown'
            for row in view['runs']))
        (self.root / 'drop-upload').unlink()
        (self.root / 'drop').touch()
        # The reader completes the immutable input before replaying dispatch.
        self.lanes()
        (self.root / 'drop').unlink()
        repeated = self.client('dispatch', **request)
        self.assertEqual(repeated['status'], 'running', repeated)
        try:
            self.wait_writer('writer@workshop')
            self.assertEqual(len(self.lanes('writer@workshop')['runs']), 1)
            remote = self.peer_workspace / '.worktrees/feat-writer'
            self.assertEqual((remote / 'large.bin').read_bytes(), (local / 'large.bin').read_bytes())
        finally:
            self.client('cancel', id='writer@workshop', operation_id='cleanup-upload-writer')

    def test_writer_handoff_and_resume_keep_context_and_publish_each_verified_head(self):
        git, _, local = self.writer_fixture()
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='continue-writer')
        self.assertEqual(launched['status'], 'running', launched)
        self.wait_writer('writer@workshop')
        remote = self.peer_workspace / '.worktrees/feat-writer'
        (remote / 'code.txt').write_text('first implementation\n')
        git(remote, 'add', 'code.txt')
        git(remote, 'commit', '-qm', 'first implementation')
        readonly = self.client('handoff', handoff=launched['id'], prompt='fixture only',
            adapter='codex', model='fixture', mode='read_only', operation_id='continue-writer.writer')
        self.assertEqual(readonly['status'], 'running', readonly)
        self.wait_writer(readonly['id'])
        first = self.client('fetch', id='continue-writer@workshop')
        self.assertEqual(first['status'], 'ok', first)
        handed = self.client('handoff', handoff=launched['id'], prompt='fixture only',
            adapter='codex', model='fixture', task_id='writer', operation_id='handoff-writer')
        self.assertEqual(handed['status'], 'running', handed)
        try:
            self.wait_writer(handed['id'])
            fetched = self.client('fetch', id='handoff-writer@workshop')
            self.assertEqual(fetched['status'], 'ok', fetched)
            self.assertEqual(fetched['verification']['head_revision'], first['verification']['head_revision'])
            for verb in ('resume', 'handoff'):
                stale = self.client(verb, **{verb: launched['id']}, prompt='fixture only',
                    operation_id='stale-' + verb, adapter='codex', model='fixture')
                self.assertEqual(stale.get('error'), 'writer_context_superseded', stale)
            resumed = self.client('resume', resume=handed['id'], prompt='admission-slow', operation_id='resume-writer')
            self.assertIn(resumed.get('state') or resumed.get('status'), {'queued', 'running'}, resumed)
            rows = self.lanes(handed['id'])['runs']
            self.assertEqual(rows[0]['attempt'], 2)
            self.assertEqual(rows[0]['state'], 'queued')
            run_id = handed['id'].split('@')[0]
            directory = next(directory for directory in self.peer_workspace.glob('.agent-run/runs/*')
                if 'mcp-' + directory.name.rsplit('-', 1)[-1] == run_id)
            (directory / 'release').touch()
            self.wait_writer(handed['id'])
            (remote / 'code.txt').write_text('second implementation\n')
            git(remote, 'add', 'code.txt')
            git(remote, 'commit', '-qm', 'second implementation')
            again = self.client('fetch', id='handoff-writer@workshop')
            self.assertEqual(again['status'], 'ok', again)
            self.assertEqual(again['verification']['head_revision'], git(remote, 'rev-parse', 'HEAD'))
            earlier = self.client('fetch', id='continue-writer@workshop')
            self.assertEqual(earlier['verification']['head_revision'], first['verification']['head_revision'])
        finally:
            for directory in self.peer_workspace.glob('.agent-run/runs/*'):
                (directory / 'release').touch()
            self.client('cancel', id=handed['id'], operation_id='cleanup-continued-writer')

    def test_resume_reservation_blocks_takeover_until_new_attempt_is_published_and_terminal(self):
        import fabric_hosts as hosts
        _, _, local = self.writer_fixture()
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='reserved-writer')
        self.wait_writer(launched['id'])
        state = self.peer_home / '.agents/.agent-fabric'
        context = json.loads((state / 'writer-contexts/reserved-writer.json').read_text())[0]
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            federation = hosts.lane_owner()
            code = federation.code()
            with federation.database() as db:
                db.execute('INSERT INTO operations VALUES (?,?,NULL)', ('reserved-resume', '{}'))
            replacement = {**context, 'operation_id': 'replacement-writer'}
            # A live preparer holds its operation lock, so its claim is not settled.
            with federation.operation_lock('reserved-resume'):
                code.claim_context(context, expected_owner='reserved-writer', admission_operation='reserved-resume')
                with self.assertRaises(hosts.HostError) as refusal:
                    code.claim_context(replacement, expected_owner='reserved-writer')
                self.assertEqual(refusal.exception.code, 'worktree_busy')
            (state / 'operations/reserved-resume.json').write_text(json.dumps({
                'runId': launched['id'].split('@')[0], 'taskId': 'writer', 'attempt': 2}))
            with federation.database() as db:
                db.execute('UPDATE operations SET result=? WHERE id=?', ('{}', 'reserved-resume'))
            for attempt, status in [(1, 'terminal'), (2, 'running')]:
                with patch.object(federation, 'worker', return_value={'runs': [{
                        'task_id': 'writer', 'attempt': attempt, 'state': status}]}), \
                        self.assertRaises(hosts.HostError) as refusal:
                    code.claim_context(replacement, expected_owner='reserved-writer')
                self.assertEqual(refusal.exception.code, 'worktree_busy')
            with patch.object(federation, 'worker', return_value={'runs': [{
                    'task_id': 'writer', 'attempt': 2, 'state': 'terminal'}]}):
                code.claim_context(replacement, expected_owner='reserved-writer')
            self.assertFalse(code.context_owner(Path(context['worktree_path'])).is_relative_to(self.peer_workspace))
            code.rollback_claims()
            self.assertEqual(json.loads(code.context_owner(Path(context['worktree_path'])).read_text())['operation_id'],
                'reserved-writer')

    def test_writer_batch_defaults_bind_each_task_and_fetch_by_task(self):
        git, base, local = self.writer_fixture()
        other = self.workspace / '.worktrees/feat-other'
        git(self.workspace, 'worktree', 'add', '-b', 'feat/other', str(other), base)
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            mode='worktree_write', worktree=str(local), operation_id='batch-writer',
            tasks=[{'id': 'writer-a', 'prompt': 'fixture only'},
                   {'id': 'writer-b', 'prompt': 'fixture only', 'worktree': str(other)}])
        self.assertEqual(launched['status'], 'running', launched)
        try:
            for task, name in [('writer-a', 'feat-writer'), ('writer-b', 'feat-other')]:
                rows = self.wait_writer(task + '@workshop')
                remote = self.peer_workspace / '.worktrees' / name
                self.assertEqual(rows[0]['worktree'], str(remote))
                (remote / 'code.txt').write_text(task + '\n')
                git(remote, 'add', 'code.txt')
                git(remote, 'commit', '-qm', task)
                fetched = self.client('fetch', id=task + '@workshop')
                self.assertEqual(fetched['status'], 'ok', fetched)
                self.assertEqual(fetched['verification']['head_revision'], git(remote, 'rev-parse', 'HEAD'))
        finally:
            self.client('cancel', id=launched['id'], operation_id='cleanup-batch-writer')

    def test_generated_writer_task_ids_keep_dispatch_and_handoff_input_bindings(self):
        git, _, local = self.writer_fixture()
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            mode='worktree_write', worktree=str(local), prompt='fixture only', operation_id='generated-writer')
        self.wait_writer(launched['id'])
        remote = self.peer_workspace / '.worktrees/feat-writer'
        (remote / 'code.txt').write_text('generated task implementation\n')
        git(remote, 'add', 'code.txt')
        git(remote, 'commit', '-qm', 'generated task implementation')
        with self.subTest(stage='dispatch'):
            fetched = self.client('fetch', id=launched['task_id'])
            self.assertEqual(fetched['status'], 'ok', fetched)
        handed = self.client('handoff', handoff=launched['id'], task_id=launched['task_id'],
            prompt='fixture only', operation_id='generated-handoff')
        self.assertEqual(handed['status'], 'running', handed)
        self.wait_writer(handed['id'])
        self.assertNotEqual(handed['task_id'], launched['task_id'])
        for selector in (handed['task_id'], 'generated-handoff@workshop'):
            with self.subTest(stage='handoff', selector=selector):
                fetched = self.client('fetch', id=selector)
                self.assertEqual(fetched['status'], 'ok', fetched)
                self.assertEqual(fetched['verification']['input_operation_id'], 'generated-writer')

    def test_mcp_writer_dispatch_is_provider_neutral_for_codex_and_agy_chairs(self):
        git, base, local = self.writer_fixture()
        for seat in ['codex', 'agy']:
            worktree = local
            task = 'writer-' + seat
            if seat == 'agy':
                worktree = self.workspace / '.worktrees/feat-agy-writer'
                git(self.workspace, 'worktree', 'add', '-b', 'feat/agy-writer', str(worktree), base)
            launched = self.mcp('fabric_dispatch', {'host': 'workshop', 'adapter': 'codex', 'model': 'fixture',
                'prompt': 'fixture only', 'mode': 'worktree_write', 'worktree': str(worktree),
                'task_id': task, 'operation_id': 'mcp-' + task, 'detail': 'full'}, seat)
            self.assertEqual(launched['status'], 'running', launched)
            try:
                self.wait_writer(task + '@workshop')
                remote = self.peer_workspace / '.worktrees' / worktree.name
                (remote / 'code.txt').write_text(seat + ' implementation\n')
                git(remote, 'add', 'code.txt')
                git(remote, 'commit', '-qm', seat + ' implementation')
                fetched = self.mcp('fabric_fetch', {'id': 'mcp-' + task + '@workshop'}, seat)
                self.assertEqual(fetched['status'], 'ok', fetched)
                self.assertEqual(fetched['verification']['head_revision'], git(remote, 'rev-parse', 'HEAD'))
            finally:
                self.client('cancel', id=task + '@workshop', operation_id='cleanup-' + task)

    def test_peer_git_refuses_unconfigured_projects_arbitrary_fields_and_invalid_metadata(self):
        import base64
        _, base, _ = self.writer_fixture()
        meta = {'base_revision': base, 'branch': 'feat/writer', 'worktree': 'feat-writer',
            'size': 1, 'sha256': 'a' * 64}
        request = {'operation_id': 'boundary-0', 'offset': 0,
            'data': base64.b64encode(b'x').decode(), 'metadata': meta}
        for changed, code in [
            ({**request, 'ref': 'refs/heads/main'}, 'invalid_request'),
            ({**request, 'offset': True}, 'invalid_code_transport'),
            ({**request, 'metadata': {**meta, 'base_revision': 'HEAD'}}, 'invalid_revision'),
            ({**request, 'metadata': {**meta, 'branch': '--upload-pack=evil'}}, 'invalid_code_transport'),
            ({**request, 'metadata': {**meta, 'worktree': '../outside'}}, 'invalid_code_transport'),
            ({**request, 'metadata': {**meta, 'size': 64 * 1024 * 1024 + 1}}, 'code_transport_too_large'),
            ({**request, 'data': 'bad base64'}, 'invalid_code_transport')]:
            with self.subTest(code=code):
                self.assertEqual(self.peer('git-upload', **changed)['error']['code'], code)
        accepted = self.peer('git-upload', **request)
        self.assertTrue(accepted['ok'], accepted)
        self.assertTrue(self.peer('git-upload', **request)['ok'])
        conflicting = {**request, 'data': base64.b64encode(b'y').decode()}
        self.assertEqual(self.peer('git-upload', **conflicting)['error']['code'], 'operation_conflict')
        config_path = self.peer_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(config_path.read_text())
        config.pop('projects')
        config_path.write_text(json.dumps(config))
        self.assertEqual(self.peer('git-upload', **request)['error']['code'], 'project_not_configured')
        self.assertEqual(self.peer('git-have', candidates=[base])['error']['code'], 'project_not_configured')

    def test_peer_git_have_offers_only_ref_reachable_candidates(self):
        git, base, _ = self.writer_fixture()
        missing = 'f' * len(base)
        for changed, code in [
            ({'candidates': [base] * 65}, 'invalid_code_transport'),
            ({'candidates': ['HEAD']}, 'invalid_revision'),
            ({'candidates': [base], 'ref': 'refs/heads/main'}, 'invalid_request')]:
            with self.subTest(code=code):
                self.assertEqual(self.peer('git-have', **changed)['error']['code'], code)
        self.assertEqual(self.peer('git-have', candidates=[missing, base])['result']['have'], [])
        git(self.peer_workspace, 'fetch', '-q', str(self.workspace), 'HEAD:refs/heads/main')
        self.assertEqual(self.peer('git-have', candidates=[missing, base])['result']['have'], [base])
        # An object that no ref reaches is never offered as a bundle prerequisite.
        dangling = git(self.peer_workspace, 'commit-tree', base + '^{tree}', '-p', base, '-m', 'dangling')
        self.assertEqual(self.peer('git-have', candidates=[dangling, base])['result']['have'], [base])

    def test_writer_bundles_only_history_the_peer_lacks(self):
        git, _, local = self.writer_fixture()
        (local / 'large.bin').write_bytes(os.urandom(200_000))
        git(local, 'add', 'large.bin')
        git(local, 'commit', '-qm', 'history the peer already holds')
        git(self.peer_workspace, 'fetch', '-q', str(local), 'HEAD:refs/heads/main')
        (local / 'new.txt').write_text('new chair commit\n')
        git(local, 'add', 'new.txt')
        git(local, 'commit', '-qm', 'chair base the peer lacks')
        base = git(local, 'rev-parse', 'HEAD')
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='thin-writer')
        self.assertEqual(launched['status'], 'running', launched)
        sent = json.loads(next(self.local_home.rglob('thin-writer-0/outgoing.json')).read_text())['metadata']
        self.assertLess(sent['size'], 50_000)
        # Each side drops its copy of the committed input once the peer consumed it.
        self.assertEqual(list(self.local_home.rglob('thin-writer-0/input.bundle')), [])
        self.assertEqual(list(self.peer_home.rglob('thin-writer-0/input.bundle')), [])
        self.assertEqual(len(list(self.peer_home.rglob('thin-writer-0/consumed'))), 1)
        remote = self.peer_workspace / '.worktrees/feat-writer'
        self.assertEqual(git(remote, 'rev-parse', 'HEAD'), base)
        self.assertEqual(len((remote / 'large.bin').read_bytes()), 200_000)
        self.wait_writer('writer@workshop')
        (remote / 'result.txt').write_text('remote implementation\n')
        git(remote, 'add', 'result.txt')
        git(remote, 'commit', '-qm', 'remote implementation')
        head = git(remote, 'rev-parse', 'HEAD')
        fetched = self.client('fetch', id='thin-writer@workshop')
        self.assertEqual(fetched['status'], 'ok', fetched)
        self.assertEqual(git(self.workspace, 'rev-parse', fetched['ref']), head)
        returned = next(self.peer_home.rglob('result.bundle'))
        self.assertLess(returned.stat().st_size, 50_000)
        self.assertIn(('-' + base).encode(), returned.read_bytes().split(b'\n\n', 1)[0])
        # The digest names the one explicit step that lands the verified head.
        self.assertIn(fetched['merge_command'], fetched['digest'])
        subprocess.run(shlex.split(fetched['merge_command']), check=True, capture_output=True)
        self.assertEqual(git(local, 'rev-parse', 'HEAD'), head)

    def test_peer_upload_retry_survives_an_interrupted_chunk_write(self):
        import base64
        import hashlib
        _, base, _ = self.writer_fixture()
        data = os.urandom(32 * 1024 + 10)
        meta = {'base_revision': base, 'branch': 'feat/writer', 'worktree': 'feat-writer',
            'size': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
        def upload(offset, chunk):
            return self.peer('git-upload', operation_id='partial-0', metadata=meta, offset=offset,
                data=base64.b64encode(chunk).decode())
        self.assertEqual(upload(0, data[:100])['error']['code'], 'invalid_code_transport')
        self.assertTrue(upload(0, data[:32 * 1024])['ok'])
        parts = next(self.peer_home.rglob('partial-0/chunks'))
        # An interrupted write leaves only an unpublished temporary file behind.
        (parts / '.partial-interrupted').write_bytes(data[32 * 1024:32 * 1024 + 3])
        for _ in range(2):
            self.assertTrue(upload(32 * 1024, data[32 * 1024:])['ok'])
        self.assertEqual(upload(32 * 1024, b'x' * 10)['error']['code'], 'operation_conflict')
        self.assertEqual(b''.join(part.read_bytes() for part in sorted(parts.glob('[0-9]*'))), data)

    def test_chunk_and_bundle_publication_failures_leave_no_temporary_files(self):
        import base64
        import hashlib
        import stat
        import fabric_hosts as hosts
        _, base, _ = self.writer_fixture()
        data = b'complete bytes for publication\n'
        meta = dict(base_revision=base, branch='feat/writer', worktree='feat-writer',
            size=len(data), sha256=hashlib.sha256(data).hexdigest())
        fsync, replace = os.fsync, os.replace
        for failure in ('file_sync', 'directory_sync', 'replace'):
            def fail_sync(fd):
                directory = stat.S_ISDIR(os.fstat(fd).st_mode)
                if (failure == 'file_sync' and not directory or failure == 'directory_sync' and directory
                        and sync_parent.exists() and os.fstat(fd).st_ino == sync_parent.stat().st_ino):
                    raise OSError('sync failed')
                return fsync(fd)
            def fail_replace(*args, **kwargs):
                if failure == 'replace':
                    raise OSError('replace failed')
                return replace(*args, **kwargs)
            with self.subTest(failure=failure), patch.dict(os.environ, {**self.env,
                    'HOME': str(self.peer_home), 'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
                code = hosts.lane_owner().code()
                identifier = 'sync-' + failure
                request = dict(operation_id=identifier, metadata=meta, offset=0,
                    data=base64.b64encode(data).decode())
                path = code.directory('Repos/project', identifier, receiving=True)
                code.write_json(path / 'incoming.json', meta)
                sync_parent = path / 'chunks'
                with patch.object(os, 'fsync', side_effect=fail_sync), \
                        patch.object(os, 'replace', side_effect=fail_replace), self.assertRaises(OSError):
                    code.peer('git-upload', 'Repos/project', request)
                self.assertEqual((path / 'chunks/00000').exists(), failure == 'directory_sync')
                self.assertEqual(list((path / 'chunks').glob('.partial-*')), [])
                if failure == 'directory_sync':
                    with patch.object(os, 'fsync', side_effect=fail_sync), self.assertRaises(OSError):
                        code.peer('git-upload', 'Repos/project', request)
                self.assertEqual(code.peer('git-upload', 'Repos/project', request)['status'], 'ok')
                sync_parent = path
                with patch.object(os, 'fsync', side_effect=fail_sync), \
                        patch.object(os, 'replace', side_effect=fail_replace), self.assertRaises(OSError):
                    code.assemble(path, meta)
                self.assertEqual((path / 'input.bundle').exists(), failure == 'directory_sync')
                self.assertEqual(list(path.glob('.bundle-*')), [])
                if failure == 'directory_sync':
                    with patch.object(os, 'fsync', side_effect=fail_sync), self.assertRaises(OSError):
                        code.assemble(path, meta)
                self.assertEqual(code.assemble(path, meta).read_bytes(), data)

    def test_peer_transport_runs_no_repository_hooks_fsmonitor_or_filters(self):
        git, _, local = self.writer_fixture()
        ran = self.root / 'repository-programs-ran'
        programs = self.root / 'repository-programs'
        programs.mkdir()
        def program(name, body=''):
            path = programs / name
            path.write_text(f'#!/bin/sh\necho {name} >> {shlex.quote(str(ran))}\n{body}')
            path.chmod(0o755)
            return str(path)
        program('post-checkout')
        program('reference-transaction')
        git(self.peer_workspace, 'config', 'core.hooksPath', str(programs))
        git(self.peer_workspace, 'config', 'core.fsmonitor', program('fsmonitor'))
        git(self.peer_workspace, 'config', 'filter.fixture.smudge', program('smudge', 'cat\n'))
        git(self.peer_workspace, 'config', 'filter.fixture.clean', program('clean', 'cat\n'))
        git(self.peer_workspace, 'config', 'filter.fixture.required', 'true')
        (local / '.gitattributes').write_text('*.txt filter=fixture\n')
        git(local, 'add', '.gitattributes')
        git(local, 'commit', '-qm', 'filtered text')
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='hardened-writer')
        self.assertEqual(launched['status'], 'running', launched)
        try:
            self.wait_writer('writer@workshop')
            remote = self.peer_workspace / '.worktrees/feat-writer'
            self.assertEqual((remote / 'code.txt').read_text(), 'committed base\n')
            created = ran.read_text().split() if ran.exists() else []
            self.assertNotIn('post-checkout', created)
            self.assertNotIn('smudge', created)
            (remote / 'result.txt').write_text('remote implementation\n')
            git(remote, 'add', 'result.txt')
            git(remote, 'commit', '-qm', 'remote implementation')
            ran.unlink(missing_ok=True)
            fetched = self.client('fetch', id='hardened-writer@workshop')
            self.assertEqual(fetched['status'], 'ok', fetched)
            self.assertFalse(ran.exists(), ran.read_text() if ran.exists() else '')
        finally:
            self.client('cancel', id='writer@workshop', operation_id='cleanup-hardened-writer')

    def test_dead_peer_preparation_settles_and_releases_its_worktree_claim(self):
        import sqlite3
        _, _, local = self.writer_fixture()
        request = dict(host='workshop', adapter='codex', model='fixture', prompt='fixture only',
            mode='worktree_write', worktree=str(local), task_id='writer')
        first = self.client('dispatch', **request, operation_id='settled-writer')
        self.assertEqual(first['status'], 'running', first)
        try:
            self.wait_writer(first['id'])
            state = self.peer_home / '.agents/.agent-fabric'
            marker = next((state / 'writer-owners').glob('*.json'))
            owner = json.loads(marker.read_text())
            def died(identifier):
                # The preparer recorded intent and claimed the worktree, then stopped.
                db = sqlite3.connect(state / 'hosts-state.sqlite3')
                with db:
                    db.execute('INSERT INTO operations VALUES (?,?,NULL)', (identifier, json.dumps(
                        {'project': 'Repos/project', 'verb': 'dispatch', 'digest': 'x'}, sort_keys=True)))
                db.close()
                marker.write_text(json.dumps({'operation_id': identifier,
                    'worktree_path': owner['worktree_path'], 'previous': owner}))
            died('dead-writer')
            import fcntl
            with (state / 'operations/dead-writer.lock').open('a') as lock:
                # An orphaned worker that still shares the lock may yet launch.
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                self.assertEqual(self.peer('operation', operation_id='dead-writer')['result']['status'], 'launch_unknown')
                self.assertEqual(json.loads(marker.read_text())['operation_id'], 'dead-writer')
            settled = self.peer('operation', operation_id='dead-writer')['result']
            self.assertEqual(settled['error'], 'launch_not_started', settled)
            self.assertEqual(json.loads(marker.read_text()), owner)
            died('dead-again')
            second = self.client('dispatch', **request, operation_id='after-dead-writer')
            self.assertEqual(second['status'], 'running', second)
            self.wait_writer(second['id'])
            self.assertEqual(self.peer('operation', operation_id='dead-again')['result']['error'], 'launch_not_started')
        finally:
            self.client('cancel', id=first['id'], operation_id='cleanup-settled-writer')
            if 'second' in locals():
                self.client('cancel', id=second['id'], operation_id='cleanup-after-dead-writer')

    def test_rejected_writer_worktree_can_be_reused_only_at_exact_clean_base(self):
        self.set_peer_response_deadline(30)
        git, base, local = self.writer_fixture()
        request = dict(host='workshop', adapter='codex', model='fixture', prompt='reject-before-attempt',
            mode='worktree_write', worktree=str(local), task_id='writer')
        first = self.client('dispatch', **request, operation_id='refused-writer')
        self.assertEqual(first['status'], 'running', first)
        try:
            self.wait_writer(first['id'])
            next_run = self.client('dispatch', **{**request, 'prompt': 'fixture only'}, operation_id='retry-refused-writer')
            self.assertEqual(next_run['status'], 'running', next_run)
            self.wait_writer(next_run['id'])
            remote = self.peer_workspace / '.worktrees/feat-writer'
            self.assertEqual(git(remote, 'rev-parse', 'HEAD'), base)
            self.assertEqual(self.client('fetch', id='refused-writer@workshop')['status'], 'rejected')
            (remote / 'code.txt').write_text('changed peer worktree\n')
            refused = self.client('dispatch', **request, operation_id='changed-context-writer')
            self.assertEqual(refused['error'], 'worktree_invalid', refused)
        finally:
            self.client('cancel', id=first['id'], operation_id='cleanup-refused-writer')
            if 'next_run' in locals():
                self.client('cancel', id=next_run['id'], operation_id='cleanup-retry-refused-writer')

    def test_clean_peer_writer_worktree_fast_forwards_to_advanced_transported_base(self):
        self.set_peer_response_deadline(30)
        git, base, local = self.writer_fixture()
        request = dict(host='workshop', adapter='codex', model='fixture', prompt='fixture only',
            mode='worktree_write', worktree=str(local), task_id='writer')
        first = self.client('dispatch', **request, operation_id='writer-before-fast-forward')
        self.assertEqual(first['status'], 'running', first)
        remote = self.peer_workspace / '.worktrees/feat-writer'
        try:
            self.wait_writer(first['id'])
            self.assertEqual(git(remote, 'rev-parse', 'HEAD'), base)
            (local / 'code.txt').write_text('advanced transported base\n')
            git(local, 'add', 'code.txt')
            git(local, 'commit', '-qm', 'advance transported base')
            advanced = git(local, 'rev-parse', 'HEAD')

            next_run = self.client('dispatch', **request, operation_id='writer-fast-forward')
            self.assertEqual(next_run['status'], 'running', next_run)
            self.assertEqual(git(remote, 'rev-parse', 'HEAD'), advanced)
            self.assertEqual(git(remote, 'symbolic-ref', '--short', 'HEAD'), 'feat/writer')
            self.assertEqual((remote / 'code.txt').read_text(), 'advanced transported base\n')
            self.assertEqual(git(remote, 'status', '--porcelain'), '')
            self.wait_writer(next_run['id'])
        finally:
            self.client('cancel', id=first['id'], operation_id='cleanup-writer-before-fast-forward')
            if 'next_run' in locals() and next_run.get('id'):
                self.client('cancel', id=next_run['id'], operation_id='cleanup-writer-fast-forward')

    def test_chair_refuses_changed_verification_bindings_and_forged_bundle_head(self):
        import copy
        import hashlib
        import fabric_hosts as hosts
        git, _, local = self.writer_fixture()
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='forged-writer')
        self.assertEqual(launched['status'], 'running', launched)
        try:
            self.wait_writer('writer@workshop')
            remote = self.peer_workspace / '.worktrees/feat-writer'
            (remote / 'code.txt').write_text('verified implementation\n')
            git(remote, 'add', 'code.txt')
            git(remote, 'commit', '-qm', 'verified implementation')
            original = self.peer('git-result', id='forged-writer')['result']
            for changed in [{'host': 'laptop'}, {'base_revision': 'a' * 40}, {'worktree': 'feat-other'},
                            {'operation_id': 'another-writer'}, {'head_revision': 'f' * 40}]:
                forged = copy.deepcopy(original)
                forged['verification'].update(changed)
                receipt = forged['verification']
                receipt['result_id'] = hashlib.sha256((receipt['operation_id'] + ':' + receipt['transfer_id']
                    + ':' + receipt['head_revision']).encode()).hexdigest()
                def exchange(verb, params=None, **kwargs):
                    if verb == 'git-result':
                        return hosts.envelope(forged)
                    # The malicious peer supplies the real bundle under the forged identity.
                    return self.peer('git-download', operation_id=original['verification']['result_id'],
                        offset=params['input']['offset'])
                with self.subTest(changed=changed), patch.dict(os.environ, self.env), \
                        patch.object(hosts.PeerClient, 'call', side_effect=exchange):
                    federation = hosts.lane_owner()
                    with self.assertRaises(hosts.HostError) as refusal:
                        federation.client_call('fetch', str(self.workspace), {'id': 'forged-writer@workshop'})
                    self.assertIn(refusal.exception.code, {'bad_response', 'writer_head_mismatch'})
                ref = 'refs/provenant/hosts/workshop/' + receipt['result_id'] + '/result'
                self.assertEqual(git(self.workspace, 'for-each-ref', '--format=%(refname)', ref), '')
        finally:
            self.client('cancel', id='writer@workshop', operation_id='cleanup-forged-writer')

    def test_fetch_cannot_substitute_another_writers_frozen_input(self):
        import copy
        import hashlib
        import fabric_hosts as hosts
        git, _, local = self.writer_fixture()
        first = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer-a', operation_id='input-a')
        self.wait_writer(first['id'])
        other = self.workspace / '.worktrees/feat-other'
        git(self.workspace, 'worktree', 'add', '--orphan', '-b', 'feat/other', str(other))
        (other / 'other.txt').write_text('unrelated input\n')
        git(other, 'add', 'other.txt')
        git(other, 'commit', '-qm', 'unrelated root')
        second = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(other),
            task_id='writer-b', operation_id='input-b')
        self.wait_writer(second['id'])
        remote = self.peer_workspace / '.worktrees/feat-other'
        (remote / 'other.txt').write_text('unrelated implementation\n')
        git(remote, 'add', 'other.txt')
        git(remote, 'commit', '-qm', 'unrelated implementation')
        original = self.peer('git-result', id='input-b')['result']
        forged = copy.deepcopy(original)
        receipt = forged['verification']
        receipt['operation_id'] = 'input-a'
        receipt['result_id'] = hashlib.sha256(('input-a:' + receipt['transfer_id'] + ':'
            + receipt['head_revision']).encode()).hexdigest()
        # Advertise the forged operation while retaining B's packed history.
        state = self.peer_home / '.agents/.agent-fabric/code'
        bundle = next(state.glob('*/' + original['verification']['result_id'] + '/result.bundle')).read_bytes()
        bundle = bundle.replace(original['verification']['result_id'].encode(), receipt['result_id'].encode())
        forged.update(size=len(bundle), sha256=hashlib.sha256(bundle).hexdigest())
        import base64
        def exchange(verb, params=None, **kwargs):
            if verb == 'git-result':
                return hosts.envelope(forged)
            offset = params['input']['offset']
            return hosts.envelope({'offset': offset, 'data': base64.b64encode(bundle[offset:offset + 32768]).decode()})
        with patch.dict(os.environ, self.env), patch.object(hosts.PeerClient, 'call', side_effect=exchange):
            with self.assertRaises(hosts.HostError) as refusal:
                hosts.lane_owner().client_call('fetch', str(self.workspace), {'id': 'input-a@workshop'})
            self.assertEqual(refusal.exception.code, 'bad_response')

    def test_fetch_quarantines_hidden_objects_and_rejected_unrelated_heads(self):
        import base64
        import copy
        import hashlib
        import fabric_hosts as hosts
        git, _, local = self.writer_fixture()
        launched = self.client('dispatch', host='workshop', adapter='codex', model='fixture',
            prompt='fixture only', mode='worktree_write', worktree=str(local),
            task_id='writer', operation_id='quarantine-writer')
        self.wait_writer(launched['id'])
        remote = self.peer_workspace / '.worktrees/feat-writer'
        (remote / 'code.txt').write_text('approved implementation\n')
        git(remote, 'add', 'code.txt')
        git(remote, 'commit', '-qm', 'approved implementation')
        original = self.peer('git-result', id='quarantine-writer')['result']
        git(remote, 'checkout', '--orphan', 'hidden-root')
        (remote / 'hidden.txt').write_text('unadvertised content\n')
        git(remote, 'add', 'hidden.txt')
        git(remote, 'commit', '-qm', 'unrelated hidden root')
        hidden = git(remote, 'rev-parse', 'HEAD')
        hidden_blob = git(remote, 'rev-parse', 'HEAD:hidden.txt')
        git(remote, 'checkout', 'feat/writer')
        for approved in (False, True):
            with self.subTest(approved=approved):
                forged = copy.deepcopy(original)
                receipt = forged['verification']
                if not approved:
                    receipt['head_revision'] = hidden
                    receipt['result_id'] = hashlib.sha256((receipt['operation_id'] + ':' + receipt['transfer_id']
                        + ':' + hidden).encode()).hexdigest()
                source = 'refs/provenant/hosts/workshop/' + receipt['result_id'] + '/result'
                git(remote, 'update-ref', source, receipt['head_revision'])
                git(remote, 'update-ref', 'refs/hidden-extra', hidden)
                bundle_path = self.root / 'malicious.bundle'
                git(remote, 'bundle', 'create', str(bundle_path), source, 'refs/hidden-extra')
                bundle = bundle_path.read_bytes().replace((hidden + ' refs/hidden-extra\n').encode(), b'')
                forged.update(size=len(bundle), sha256=hashlib.sha256(bundle).hexdigest())
                def exchange(verb, params=None, **kwargs):
                    if verb == 'git-result':
                        return hosts.envelope(forged)
                    offset = params['input']['offset']
                    return hosts.envelope({'offset': offset, 'data': base64.b64encode(bundle[offset:offset + 32768]).decode()})
                with patch.dict(os.environ, self.env), patch.object(hosts.PeerClient, 'call', side_effect=exchange):
                    federation = hosts.lane_owner()
                    if approved:
                        result = federation.client_call('fetch', str(self.workspace), {'id': 'quarantine-writer@workshop'})
                        self.assertEqual(result['status'], 'ok', result)
                        self.assertEqual(git(self.workspace, 'rev-parse', result['ref']), receipt['head_revision'])
                    else:
                        with self.assertRaises(hosts.HostError):
                            federation.client_call('fetch', str(self.workspace), {'id': 'quarantine-writer@workshop'})
                        self.assertEqual(git(self.workspace, 'for-each-ref', '--format=%(refname)', source), '')
                for oid in (hidden, hidden_blob):
                    found = subprocess.run(['git', '-C', str(self.workspace), 'cat-file', '-e', oid], capture_output=True)
                    self.assertNotEqual(found.returncode, 0, 'unapproved object escaped quarantine: ' + oid)

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

    def test_state_filter_applies_after_selector_identity_and_ambiguity(self):
        selected = self.client('lanes', ids=['shared-task@workshop'], state='active')
        self.assertEqual(selected['status'], 'ok')
        self.assertEqual(selected['runs'], [])
        ambiguous = self.client('lanes', ids=['shared-task'], state='active')
        self.assertEqual(ambiguous['error'], 'ambiguous_selector')

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

    def test_selector_reads_do_not_transfer_unrelated_large_history(self):
        for index in range(40):
            self.terminal(self.peer_workspace, f'history-{index}', f'history-{index}')
        for path in self.peer_workspace.glob('.agent-run/runs/*history-*/tasks/*/attempt-001/attempt.json'):
            row = json.loads(path.read_text())
            row['digest'] = 'unrelated retained history ' * 2000
            path.write_text(json.dumps(row))
        selected = self.client('status', ids=['shared-task@workshop'], detail='full')
        self.assertEqual(selected['runs'][0]['reachability'], 'reachable')
        self.assertEqual(selected['runs'][0]['task_id'], 'shared-task@workshop')
        self.assertLess(len(json.dumps(selected)), 20000)
        self.terminal(self.workspace, 'local-only', 'local-only')
        self.terminal(self.peer_workspace, 'peer-only', 'peer-only')
        # A miss on one owner must not suppress another selector on that owner.
        mixed = self.client('status', ids=['local-only', 'peer-only@workshop', 'shared-task@workshop'])
        self.assertEqual({row['task_id'] for row in mixed['runs']},
            {'local-only@laptop', 'peer-only@workshop', 'shared-task@workshop'})

    def test_selector_observation_preserves_other_snapshot_ages(self):
        import fabric_hosts as hosts
        old = {'id': 'mcp-older', 'run_id': 'mcp-older', 'task_id': 'older',
            'attempt': 1, 'state': 'terminal', 'status': 'ok'}
        new = {**old, 'id': 'mcp-newer', 'run_id': 'mcp-newer', 'task_id': 'newer'}
        with patch.dict(os.environ, self.env):
            federation = hosts.lane_owner()
            with patch.object(federation, 'worker', return_value={'status': 'ok', 'runs': []}), \
                    patch.object(hosts.PeerClient, 'call', return_value=hosts.envelope({'status': 'ok', 'runs': [old, new]})), \
                    patch('time.time', return_value=100):
                federation.reads('lanes', 'Repos/project', {})
            with patch.object(hosts.PeerClient, 'call', return_value=hosts.envelope({'status': 'ok', 'runs': [new]})), \
                    patch('time.time', return_value=200):
                federation.reads('lanes', 'Repos/project', {'ids': ['newer@workshop']})
            with patch.object(hosts.PeerClient, 'call', return_value=hosts.envelope(error=hosts.HostError('unreachable', 'offline'))), \
                    patch('time.time', return_value=300):
                stale = federation.reads('lanes', 'Repos/project', {'ids': ['older@workshop', 'newer@workshop']})
            ages = {row['task_id']: row['age_seconds'] for row in stale['runs']}
            self.assertEqual(ages, {'older@workshop': 200, 'newer@workshop': 100})

    def test_reachable_owner_read_failure_preserves_typed_error_and_snapshot(self):
        import fabric_hosts as hosts
        row = {'id': self.peer_run, 'run_id': self.peer_run, 'task_id': 'shared-task',
            'attempt': 1, 'state': 'terminal', 'status': 'ok'}
        with patch.dict(os.environ, self.env):
            federation = hosts.lane_owner()
            with patch.object(hosts.PeerClient, 'call', return_value=hosts.envelope({'status': 'ok', 'runs': [row]})):
                federation.reads('status', 'Repos/project', {'ids': ['shared-task@workshop']})
            failed = {'reachability': 'reachable', **hosts.envelope(error=hosts.HostError('lane_owner_failed', 'bounded owner response'))}
            with patch.object(hosts.PeerClient, 'call', return_value=failed):
                view = federation.reads('status', 'Repos/project', {'ids': ['shared-task@workshop', 'not-cached@workshop']})
            self.assertEqual(view['hosts'][0]['reachability'], 'reachable')
            self.assertEqual(len(view['runs']), 2)
            for result in view['runs']:
                self.assertEqual(result['state'], 'read_error')
                self.assertEqual(result['reachability'], 'reachable')
                self.assertEqual(result['error']['code'], 'lane_owner_failed')
                self.assertIn('lane_owner_failed', result['digest'])
            self.assertEqual(view['runs'][0]['last_known_state'], 'terminal')

    def test_pending_default_launch_cannot_fall_back_when_retried_offline(self):
        import fabric_hosts as hosts
        self.writer_fixture()
        with patch.dict(os.environ, self.env):
            federation = hosts.lane_owner()
            request = {'prompt': 'fixture only', 'operation_id': 'pending-default'}
            def sent(action, params=None, **kwargs):
                if action == 'git-have':
                    return hosts.envelope({'have': []})
                if action in {'hello', 'git-upload'}:
                    return hosts.envelope({'status': 'ok'})
                return hosts.envelope(error=hosts.HostError('timeout', 'response lost'))
            with patch.object(hosts.PeerClient, 'call', side_effect=sent):
                first = federation.client_call('dispatch', str(self.workspace), request)
            self.assertEqual(first['state'], 'launch_unknown')
            offline = {'reachability': 'unreachable', **hosts.envelope(error=hosts.HostError('unreachable', 'offline'))}
            with patch.object(hosts.PeerClient, 'call', return_value=offline):
                retried = federation.client_call('dispatch', str(self.workspace), request)
            self.assertEqual(retried.get('state'), 'launch_unknown')
            self.assertNotIn('local', retried)
            config_path = self.local_home / '.agents/.agent-fabric/hosts.json'
            config = json.loads(config_path.read_text())
            for peers in [config['peers'], {}]:
                config['peers'] = peers
                config['projects']['Repos/project']['default_host'] = 'laptop'
                config_path.write_text(json.dumps(config))
                with self.assertRaises(hosts.HostError) as raised:
                    hosts.lane_owner().client_call('dispatch', str(self.workspace), request)
                self.assertEqual(raised.exception.code, 'operation_conflict')

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
        self.writer_fixture()
        with patch.dict(os.environ, self.env):
            chair = hosts.lane_owner()
            client = hosts.PeerClient('workshop', chair.config.peers['workshop'])
            request = chair.code().outgoing('Repos/project', {'operation_id': 'prelaunch-failure',
                'prompt': 'ordinary fixture input'}, str(self.workspace), 'prelaunch-failure', 'workshop', client)
            chair.code().send(client, 'Repos/project', request['code_transport'])
        with patch.dict(os.environ, {**self.env, 'HOME': str(self.peer_home),
                'AGENT_FABRIC_INSTANCE_ROOT': str(self.peer_home / '.agents')}):
            federation = hosts.lane_owner()
            params = {'project_path': 'Repos/project', 'input': request}
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

    def owner_process(self, script, cwd=None):
        """Run like the dispatch owner: fabric_hosts loaded by path, scripts/ not on sys.path."""
        program = ('import importlib.util,json,sys\n'
            'sys.path[:] = [p for p in sys.path if p not in {"", ' + repr(str(ROOT / 'scripts')) + '}]\n'
            'spec = importlib.util.spec_from_file_location("provenant_fabric_hosts", '
            + repr(str(ROOT / 'scripts/fabric_hosts.py')) + ')\n'
            'hosts = importlib.util.module_from_spec(spec); sys.modules[spec.name] = hosts\n'
            'spec.loader.exec_module(hosts)\n' + script)
        process = subprocess.run([sys.executable, '-c', program], cwd=cwd or self.workspace, env=self.env,
            capture_output=True, text=True, timeout=30)
        self.assertEqual(process.returncode, 0, process.stderr)
        return json.loads(process.stdout)

    def test_owner_process_resolves_project_identity_without_scripts_on_path(self):
        value = self.owner_process('print(json.dumps(hosts.project_identity()))')
        self.assertEqual(value, 'Repos/project')

    def broken_config_dispatch(self, mutate, **request):
        config_path = self.local_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(config_path.read_text())
        mutate(config)
        config_path.write_text(json.dumps(config))
        return self.owner_process('print(json.dumps(hosts.lane_owner().client_call("dispatch", '
            + repr(str(self.workspace)) + ', ' + repr({"prompt": "placement only", **request}) + ')))')

    def test_owner_process_runs_locally_when_hosts_config_is_unparseable(self):
        (self.local_home / '.agents/.agent-fabric/hosts.json').write_text('{not json')
        value = self.owner_process('print(json.dumps(hosts.lane_owner().client_call("dispatch", '
            + repr(str(self.workspace)) + ', {"prompt": "placement only"})))')
        self.assertTrue(value['local'])
        self.assertEqual(value['fallback']['reason'], 'invalid_config')

    def test_invalid_peer_config_falls_back_locally_with_visible_detail(self):
        def mutate(config):
            config['peers'][next(iter(config['peers']))]['response_deadline'] = 0
        value = self.broken_config_dispatch(mutate)
        self.assertTrue(value['local'])
        self.assertEqual(value['fallback']['reason'], 'invalid_config')
        self.assertIn('Response deadline', value['fallback']['detail'])
        self.assertIn('invalid_config', value['digest'])

    def test_invalid_project_key_falls_back_locally_instead_of_rejecting(self):
        def mutate(config):
            config['projects'] = {'/absolute/project': {'default_host': config['local_host']}}
        for request in [{}, {'mode': 'worktree_write'}]:
            with self.subTest(request=request):
                value = self.broken_config_dispatch(mutate, **request)
                self.assertTrue(value['local'])
                self.assertEqual(value['fallback']['reason'], 'invalid_config')

    def test_explicit_remote_request_stays_fail_closed_with_invalid_config(self):
        def mutate(config):
            config['peers'][next(iter(config['peers']))]['response_deadline'] = 0
        peer = next(iter(json.loads((self.local_home / '.agents/.agent-fabric/hosts.json').read_text())['peers']))
        config_path = self.local_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(config_path.read_text()); mutate(config); config_path.write_text(json.dumps(config))
        process = subprocess.run([str(ROOT / 'scripts/fabric-hosts'), 'lane'],
            input=json.dumps({'action': 'dispatch', 'cwd': str(self.workspace), 'input': {'host': peer, 'prompt': 'x'}}),
            cwd=self.workspace, env=self.env, capture_output=True, text=True, timeout=30)
        value = json.loads(process.stdout)
        self.assertNotIn('local', value)
        self.assertEqual(value['error'], 'unknown_host')

    def test_owner_process_runs_worktree_write_locally_when_read_only_peer_is_offline(self):
        config_path = self.local_home / '.agents/.agent-fabric/hosts.json'
        config = json.loads(config_path.read_text())
        peer = next(iter(config['peers']))
        config['projects']['Repos/project'] = {'default_host': config['local_host'], 'modes': {'read_only': peer}}
        config_path.write_text(json.dumps(config))
        self.offline.touch()
        value = self.owner_process('print(json.dumps(hosts.lane_owner().client_call("dispatch", '
            + repr(str(self.workspace)) + ', {"mode": "worktree_write", "prompt": "placement only"})))')
        self.assertTrue(value['local'])
        self.assertNotIn('fallback', value)

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

    def test_default_views_sort_globally_and_keep_unknown_hosts_visible(self):
        import fabric_hosts as hosts
        local = [{'id': f'mcp-local-{index}', 'run_id': f'mcp-local-{index}',
            'task_id': f'local-{index}', 'attempt': 1, 'state': 'terminal', 'status': 'ok',
            'started_at': '2026-10-07T00:00:00Z'} for index in range(20)]
        remote = {'id': 'mcp-newest-peer', 'run_id': 'mcp-newest-peer', 'task_id': 'newest-peer',
            'attempt': 1, 'state': 'terminal', 'status': 'ok', 'started_at': '2026-10-08T00:00:00Z'}
        with patch.dict(os.environ, self.env):
            federation = hosts.lane_owner()
            with patch.object(federation, 'worker', return_value={'status': 'ok', 'runs': local}), \
                    patch.object(hosts.PeerClient, 'call', return_value=hosts.envelope({'status': 'ok', 'runs': [remote]})):
                view = federation.reads('lanes', 'Repos/project', {})
            self.assertEqual(len(view['runs']), 20)
            self.assertEqual(view['runs'][0]['id'], 'mcp-newest-peer@workshop')
            self.assertEqual(view['omitted'], 1)
            with federation.database() as database:
                database.execute('INSERT INTO pending VALUES (?,?,?,?,?)',
                    ('visibility-unknown', 'Repos/project', 'workshop', 'dispatch',
                        json.dumps({'operation_id': 'visibility-unknown', 'prompt': 'fixture only'})))
            with patch.object(federation, 'worker', return_value={'status': 'ok', 'runs': local}), \
                    patch.object(hosts.PeerClient, 'call', return_value=hosts.envelope(
                        error=hosts.HostError('unreachable', 'fixture offline'))):
                offline = federation.reads('lanes', 'Repos/project', {})
            # A full local window cannot conceal loss of peer reachability or
            # a mutation whose owning launch outcome is still unknown.
            self.assertTrue(any(row['state'] == 'unreachable' and row['host'] == 'workshop'
                for row in offline['runs']))
            self.assertTrue(any(row['state'] == 'launch_unknown'
                and row['operation_id'] == 'visibility-unknown' for row in offline['runs']))
            self.assertEqual(len(offline['runs']), 20)

    def test_offline_terminal_window_cannot_hide_local_active_work(self):
        import fabric_hosts as hosts
        remote = [{'id': f'mcp-peer-{index}', 'run_id': f'mcp-peer-{index}',
            'task_id': f'peer-{index}', 'attempt': 1, 'state': 'terminal', 'status': 'ok',
            'started_at': '2026-10-08T00:00:00Z'} for index in range(20)]
        running = {'id': 'mcp-local-running', 'run_id': 'mcp-local-running', 'task_id': 'local-running',
            'attempt': 1, 'state': 'running', 'status': 'running', 'started_at': '2026-10-07T00:00:00Z'}
        with patch.dict(os.environ, self.env):
            federation = hosts.lane_owner()
            with patch.object(federation, 'worker', return_value={'status': 'ok', 'runs': []}), \
                    patch.object(hosts.PeerClient, 'call', return_value=hosts.envelope({'status': 'ok', 'runs': remote})):
                federation.reads('lanes', 'Repos/project', {})
            with patch.object(federation, 'worker', return_value={'status': 'ok', 'runs': [running]}), \
                    patch.object(hosts.PeerClient, 'call', return_value=hosts.envelope(error=hosts.HostError('unreachable', 'offline'))):
                view = federation.reads('lanes', 'Repos/project', {})
            self.assertTrue(any(row['state'] == 'running' and row['host'] == 'laptop' for row in view['runs']))
            self.assertTrue(any(row['state'] == 'unreachable' and row['host'] == 'workshop' for row in view['runs']))
            self.assertEqual(view['omitted'], 1)

    def test_reader_does_not_reconcile_an_inflight_control_send(self):
        import fabric_hosts as hosts
        import threading
        # Named reader dispatches freeze committed input before reaching the wire.
        self.fixture_product()
        cases = [('cancel', {'id': self.peer_run + '@workshop'}),
            ('resume', {'resume': self.peer_run + '@workshop'}),
            ('handoff', {'handoff': self.peer_run + '@workshop'}),
            ('dispatch', {'session': 'named session@workshop', 'prompt': 'fixture only'})]
        with patch.dict(os.environ, self.env):
            for verb, payload in cases:
                with self.subTest(verb=verb):
                    sender = hosts.lane_owner()
                    reader = hosts.lane_owner()  # Same durable root, distinct owner.
                    started, release, recovered = threading.Event(), threading.Event(), threading.Event()
                    calls, results, failures = [], [], []
                    identifier = 'inflight-' + verb
                    terminal = {'id': self.peer_run, 'run_id': self.peer_run,
                        'task_id': 'shared-task', 'attempt': 1, 'state': 'terminal', 'status': 'ok'}
                    def response(action, params=None, **kwargs):
                        calls.append(action)
                        if action == 'hello':
                            return hosts.envelope({'status': 'ok'})
                        if action == 'git-have':
                            return hosts.envelope({'have': []})
                        if action == verb:
                            started.set()
                            if not release.wait(5):
                                raise AssertionError('fixture send was not released')
                            return hosts.envelope(error=hosts.HostError('timeout', 'lost response after send'))
                        if action == 'operation':
                            if recovered.is_set():
                                return hosts.envelope({'status': 'ok', 'runs': [terminal]})
                            return hosts.envelope({'status': 'operation_missing', 'operation_id': identifier})
                        return hosts.envelope({'status': 'ok', 'runs': []})
                    def send():
                        try:
                            results.append(sender.client_call(verb, str(self.workspace),
                                {**payload, 'operation_id': identifier}))
                        except BaseException as exc:
                            failures.append(exc)
                    with patch.object(sender, 'worker', return_value={'status': 'ok', 'runs': []}), \
                            patch.object(reader, 'worker', return_value={'status': 'ok', 'runs': []}), \
                            patch.object(hosts.PeerClient, 'call', side_effect=response):
                        thread = threading.Thread(target=send)
                        thread.start()
                        try:
                            self.assertTrue(started.wait(2), 'send did not reach the controlled wire boundary')
                            with reader.database() as database:
                                self.assertIsNotNone(database.execute('SELECT id FROM pending WHERE id=?',
                                    (identifier,)).fetchone())
                            view = reader.reads('lanes', 'Repos/project', {})
                            self.assertNotIn('operation', calls,
                                'reader queried an operation while its original send owned the pending row')
                            self.assertEqual(calls.count(verb), 1, 'reader replayed the in-flight control')
                            with reader.database() as database:
                                self.assertIsNotNone(database.execute('SELECT id FROM pending WHERE id=?',
                                    (identifier,)).fetchone(), 'reader deleted an in-flight pending record')
                            self.assertTrue(any(row.get('operation_id') == identifier
                                and row['state'] == 'launch_unknown' for row in view['runs']))
                        finally:
                            release.set()
                            thread.join(5)
                        self.assertFalse(thread.is_alive(), 'fixture sender did not finish')
                        self.assertEqual(failures, [])
                        self.assertEqual(results[0]['state'], 'launch_unknown')
                        recovered.set()
                        view = reader.reads('lanes', 'Repos/project', {})
                        self.assertTrue(any(row['run_id'] == self.peer_run + '@workshop'
                            and row['state'] == 'terminal' for row in view['runs']))
                        with reader.database() as database:
                            self.assertIsNone(database.execute('SELECT id FROM pending WHERE id=?',
                                (identifier,)).fetchone(), 'completed reconciliation did not clear pending')

    def fixture_product(self):
        """Reuse existing owner fixtures, never dispatch a real provider."""
        import shutil
        # Remote readers now require committed input and an opted-in peer project.
        for workspace in (self.workspace, self.peer_workspace):
            if subprocess.run(['git', '-C', str(workspace), 'rev-parse', '--verify', 'HEAD'],
                    capture_output=True).returncode:
                subprocess.run(['git', '-C', str(workspace), 'commit', '--allow-empty', '-qm', 'fixture root'],
                    check=True, capture_output=True, env={**os.environ, 'GIT_AUTHOR_NAME': 'Fixture',
                        'GIT_AUTHOR_EMAIL': 'fixture@example.invalid', 'GIT_COMMITTER_NAME': 'Fixture',
                        'GIT_COMMITTER_EMAIL': 'fixture@example.invalid'})
        peer_config_path = self.peer_home / '.agents/.agent-fabric/hosts.json'
        peer_config = json.loads(peer_config_path.read_text())
        peer_config.setdefault('projects', {})['Repos/project'] = {}
        peer_config_path.write_text(json.dumps(peer_config))
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
        for name in ['run_dir_init.sh', 'dispatch_run.py', 'batch_run.py', 'run_controls.py']:
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
            + 'if os.path.exists(' + repr(str(self.offline)) + '): sys.exit(255)\n'
            + 'request=sys.stdin.buffer.read()\n'
            + 'environment={**os.environ,"HOME":' + repr(str(self.peer_home))
            + ',"AGENT_FABRIC_INSTANCE_ROOT":' + repr(str(self.peer_home / '.agents'))
            + ',"AGENT_FABRIC_PRODUCT_ROOT":' + repr(str(product))
            + ',"AGENT_FABRIC_STATE_DIRECTORY":' + repr(str(self.peer_home / 'state')) + '}\n'
            + 'result=subprocess.run([' + repr(str(ROOT / 'scripts/fabric-hosts'))
            + ',"peer"],input=request,capture_output=True,env=environment,cwd=environment["HOME"])\n'
            + 'if json.loads(request)["verb"] == "git-upload" and json.loads(request)["params"]["input"]["offset"] > 0 and os.path.exists(' + repr(str(self.root / 'drop-upload')) + '): sys.exit(255)\n'
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
