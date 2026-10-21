import http.client
import json
import os
import shutil
import socket
import stat
import subprocess
import time
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'tools/otlp-collector/otlp-collector.swift'

pytestmark = pytest.mark.skipif(shutil.which('swiftc') is None, reason='swiftc not available')


@pytest.fixture(scope='module')
def binary(tmp_path_factory):
    out = tmp_path_factory.mktemp('otlp-bin') / 'otlp-collector'
    subprocess.run(['swiftc', '-O', '-swift-version', '5', str(SOURCE), '-o', str(out)],
                   check=True, capture_output=True, timeout=120)
    return out


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def post(port, path, body, content_type):
    conn = http.client.HTTPConnection('127.0.0.1', port, timeout=5)
    conn.request('POST', path, body=body, headers={'Content-Type': content_type})
    response = conn.getresponse()
    return response.status, response.read()


def test_collector_stores_json_rejects_protobuf_and_exits_idle(binary, tmp_path):
    port = free_port()
    env = {**os.environ, 'AGENT_FABRIC_STATE_ROOT': str(tmp_path)}
    proc = subprocess.Popen([str(binary), '--listen', f'127.0.0.1:{port}', '--idle-seconds', '1.5'],
                            env=env, stdout=subprocess.PIPE, text=True)
    try:
        assert proc.stdout.readline().startswith('listening')
        body = json.dumps({'resourceLogs': [{'scopeLogs': []}]}, indent=2)
        assert post(port, '/v1/logs', body, 'application/json') == (200, b'{}')
        status, message = post(port, '/v1/logs', b'\x0a\x00', 'application/x-protobuf')
        assert status == 415 and b'http/json' in message
        assert post(port, '/v1/logs', 'not json', 'application/json')[0] == 400
        assert post(port, '/v1/other', '{}', 'application/json')[0] == 404

        files = list((tmp_path / 'otel').glob('logs-*.ndjson'))
        assert len(files) == 1
        assert stat.S_IMODE(files[0].stat().st_mode) == 0o600
        lines = files[0].read_text().splitlines()
        assert len(lines) == 1
        record = json.loads(lines[0])
        assert record['signal'] == 'logs'
        assert record['received_at'].endswith('Z')
        assert record['body'] == json.loads(body)

        assert proc.wait(timeout=6) == 0
    finally:
        if proc.poll() is None:
            proc.kill()


def test_collector_refuses_non_loopback(binary, tmp_path):
    result = subprocess.run([str(binary), '--listen', '0.0.0.0:4318'],
                            env={**os.environ, 'AGENT_FABRIC_STATE_ROOT': str(tmp_path)},
                            capture_output=True, text=True, timeout=5)
    assert result.returncode == 2 and 'loopback' in result.stderr
