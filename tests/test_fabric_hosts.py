"""Host transport contracts, exercised without an sshd or provider sign-in."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / 'scripts'))


class PeerContracts(unittest.TestCase):
    def peer(self, request, **env):
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
            environment = {**os.environ, 'HOME': directory,
                           'AGENT_FABRIC_INSTANCE_ROOT': directory, **env}
            return subprocess.run([str(ROOT / 'scripts/provenant'), 'peer'],
                                  input=request, capture_output=True, env=environment)

    def test_peer_doctor_starts_with_a_nonlogin_system_path(self):
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
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
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
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
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
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
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
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
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
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
                env={**os.environ, 'HOME':str(local),'AGENT_FABRIC_INSTANCE_ROOT':str(local / '.agents'),
                     'AGENT_FABRIC_SSH_PROGRAM':str(shim)})
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
    def test_transport_errors_and_protocol_write_decision(self):
        import fabric_hosts as hosts
        self.assertTrue(hasattr(hosts, 'PeerClient'), 'single peer client is missing')
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
            shim = Path(directory) / 'ssh'
            def client(program, deadline=1):
                shim.write_text('#!' + sys.executable + '\n' + program)
                shim.chmod(0o755)
                return hosts.PeerClient('workshop', hosts.PeerConfig('user@workshop', response_deadline=deadline), ssh_program=str(shim))
            unreachable = client('import sys; sys.exit(255)')
            self.assertEqual(unreachable.call('doctor')['error']['code'], 'unreachable')
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
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
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
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
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
            self.assertEqual(calls[0][0], ['-o', 'BatchMode=yes', '-o', 'ConnectTimeout=5', 'workshop', '.local/bin/provenant peer'])
            self.assertEqual(calls[0][1]['verb'], 'hello')


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
        with tempfile.TemporaryDirectory(dir='/tmp') as directory:
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


if __name__ == '__main__':
    unittest.main()
