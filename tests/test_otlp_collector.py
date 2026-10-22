import http.client
import json
import os
import resource
import shutil
import signal
import socket
import stat
import subprocess
import sys
import time
from contextlib import ExitStack
from datetime import datetime, timezone
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / 'tools/otlp-collector/otlp-collector.swift'

pytestmark = pytest.mark.skipif(
    sys.platform != 'darwin' or shutil.which('swiftc') is None,
    reason='collector uses launchd and Darwin APIs; needs macOS swiftc',
)


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
    try:
        conn.request('POST', path, body=body, headers={'Content-Type': content_type})
        response = conn.getresponse()
        return response.status, response.read()
    finally:
        conn.close()


@pytest.fixture
def collector(binary, tmp_path, request):
    port = free_port()
    options = getattr(request, 'param', {}).copy()
    preexec = options.pop('preexec_fn', None)
    proc = subprocess.Popen([str(binary), '--listen', f'127.0.0.1:{port}'],
                            env={**os.environ, 'AGENT_FABRIC_STATE_ROOT': str(tmp_path),
                                 **options},
                            stdout=subprocess.PIPE, text=True, preexec_fn=preexec)
    try:
        assert proc.stdout.readline().startswith('listening')
        yield port
    finally:
        proc.kill()
        proc.wait(timeout=5)


@pytest.mark.parametrize('target', ['file', 'directory'])
def test_collector_refuses_storage_symlinks(collector, tmp_path, target):
    outside = tmp_path / 'outside'
    outside.mkdir()
    original = outside / 'record'
    original.write_text('preserved\n')
    directory = tmp_path / 'otel'
    if target == 'directory':
        directory.symlink_to(outside, target_is_directory=True)
    else:
        directory.mkdir()
        day = datetime.now(timezone.utc).strftime('%Y%m%d')
        (directory / f'logs-{day}.ndjson').symlink_to(original)

    assert post(collector, '/v1/logs', '{}', 'application/json')[0] == 500
    assert original.read_text() == 'preserved\n'
    assert list(outside.iterdir()) == [original]


def test_collector_tightens_existing_storage_permissions(collector, tmp_path):
    directory = tmp_path / 'otel'
    directory.mkdir(mode=0o755)
    day = datetime.now(timezone.utc).strftime('%Y%m%d')
    daily = directory / f'logs-{day}.ndjson'
    daily.write_text('')
    daily.chmod(0o644)

    assert post(collector, '/v1/logs', '{}', 'application/json')[0] == 200
    assert stat.S_IMODE(daily.stat().st_mode) == 0o600
    assert stat.S_IMODE(directory.stat().st_mode) == 0o700


def test_collector_prunes_only_expired_regular_files(collector, tmp_path):
    directory = tmp_path / 'otel'
    directory.mkdir()
    old = directory / 'logs-20000101.ndjson'
    recent = directory / 'logs-20000102.ndjson'
    outside = tmp_path / 'outside.ndjson'
    for path in (old, recent, outside):
        path.write_text('{}\n')
    link = directory / 'traces-20000101.ndjson'
    link.symlink_to(outside)
    expired = time.time() - 31 * 86400
    os.utime(old, (expired, expired))
    os.utime(link, (expired, expired), follow_symlinks=False)

    assert post(collector, '/v1/logs', '{}', 'application/json')[0] == 200
    assert not old.exists()
    assert recent.read_text() == '{}\n'
    assert link.is_symlink()
    assert outside.read_text() == '{}\n'


def test_collector_caps_concurrent_connections(collector):
    with ExitStack() as stack:
        for _ in range(32):
            conn = http.client.HTTPConnection('127.0.0.1', collector, timeout=2)
            stack.callback(conn.close)
            conn.request('POST', '/v1/logs', '{}', {'Content-Type': 'application/json'})
            response = conn.getresponse()
            assert response.status == 200
            response.read()
        with socket.create_connection(('127.0.0.1', collector), timeout=1) as extra:
            response = http.client.HTTPResponse(extra)
            response.begin()
            assert response.status == 503
            assert response.getheader('Connection') == 'close'
            response.read()
            assert extra.recv(4096) == b''


@pytest.mark.parametrize('collector', [{'PROVENANT_OTEL_REQUEST_SECONDS': '0.25'}], indirect=True)
@pytest.mark.parametrize('phase', ['headers', 'body'])
def test_collector_deadline_expires_despite_continued_reads(collector, phase):
    with socket.create_connection(('127.0.0.1', collector), timeout=0.05) as conn:
        conn.sendall(b'POST /v1/logs HTTP/1.1\r\n')
        if phase == 'body':
            conn.sendall(b'Content-Length: 1000\r\n\r\n')
        end = time.monotonic() + 0.8
        closed = False
        while time.monotonic() < end:
            conn.sendall(b' ')
            try:
                if conn.recv(4096) == b'':
                    closed = True
                    break
            except socket.timeout:
                pass
        assert closed, 'request deadline was extended by incoming bytes'


def limit_file_size():
    signal.signal(signal.SIGXFSZ, signal.SIG_IGN)
    resource.setrlimit(resource.RLIMIT_FSIZE, (40, 40))


@pytest.mark.parametrize('collector', [{'preexec_fn': limit_file_size}], indirect=True)
def test_collector_rolls_back_short_write(collector, tmp_path):
    directory = tmp_path / 'otel'
    directory.mkdir()
    day = datetime.now(timezone.utc).strftime('%Y%m%d')
    daily = directory / f'logs-{day}.ndjson'
    original = '{"preserved":true}\n'
    daily.write_text(original)

    assert post(collector, '/v1/logs', '{}', 'application/json')[0] == 500
    assert daily.read_text() == original


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
        proc.wait(timeout=5)


def test_collector_refuses_non_loopback(binary, tmp_path):
    # A fixed port would collide with another worker if the refusal regressed and bound it.
    result = subprocess.run([str(binary), '--listen', f'0.0.0.0:{free_port()}'],
                            env={**os.environ, 'AGENT_FABRIC_STATE_ROOT': str(tmp_path)},
                            capture_output=True, text=True, timeout=5)
    assert result.returncode == 2 and 'loopback' in result.stderr
