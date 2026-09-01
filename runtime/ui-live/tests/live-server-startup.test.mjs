import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import test from 'node:test';
import assert from 'node:assert/strict';

import {
  classifyStartupOutcome,
  observeStartup,
} from '../live-server-startup.mjs';

const SERVER = fileURLToPath(new URL('../live-server.mjs', import.meta.url));
const PRODUCT_ROOT = path.resolve(path.dirname(SERVER), '..', '..');

function serverEnv() {
  const env = { ...process.env, AGENT_FABRIC_PRODUCT_ROOT: PRODUCT_ROOT };
  delete env.AGENTS_HOME;
  return env;
}

function newProject() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'impeccable-live-startup-'));
}

function stopServer(project) {
  spawnSync(process.execPath, [SERVER, 'stop', '--keep-inject'], {
    cwd: project,
    encoding: 'utf8',
    timeout: 5_000,
    env: serverEnv(),
  });
}

test('classifies a ready server as success with its observed elapsed time', () => {
  const observed = observeStartup({ startedAt: 1_000, observedAt: 1_120, baselineMs: 10_000 });

  assert.deepEqual(
    classifyStartupOutcome({ ready: true, observation: observed }),
    { status: 'success', observation: { baselineMs: 10_000, elapsedMs: 120 } },
  );
});

test('classifies an exited child as an immediate bind or startup refusal', () => {
  const observed = observeStartup({ startedAt: 1_000, observedAt: 1_035, baselineMs: 10_000 });

  assert.deepEqual(
    classifyStartupOutcome({
      exit: { code: 1, signal: null },
      observation: observed,
    }),
    {
      status: 'refused',
      observation: { baselineMs: 10_000, elapsedMs: 35 },
      exit: { code: 1, signal: null },
    },
  );
});

test('classifies an unready server at the bounded baseline as an environmental timeout', () => {
  const observed = observeStartup({ startedAt: 1_000, observedAt: 11_000, baselineMs: 10_000 });

  assert.deepEqual(
    classifyStartupOutcome({ observation: observed }),
    {
      status: 'timeout',
      classification: 'environmental_timeout',
      observation: { baselineMs: 10_000, elapsedMs: 10_000 },
    },
  );
});

test('does not classify an unready server below the baseline as an environmental timeout', () => {
  const observed = observeStartup({ startedAt: 1_000, observedAt: 9_999, baselineMs: 10_000 });

  assert.deepEqual(
    classifyStartupOutcome({ observation: observed }),
    { status: 'unclassified', observation: { baselineMs: 10_000, elapsedMs: 8_999 } },
  );
});

test('does not classify an unobserved startup as an environmental timeout', () => {
  assert.deepEqual(
    classifyStartupOutcome({}),
    { status: 'unclassified', observation: undefined },
  );
});

test('direct stop aborts an in-flight upload before removing private temporary state', async () => {
  const project = newProject();
  const launcher = spawnSync(process.execPath, [SERVER, '--background'], {
    cwd: project,
    env: serverEnv(),
    encoding: 'utf8',
    timeout: 15_000,
  });
  assert.equal(launcher.status, 0, launcher.stderr);
  const recordPath = path.join(project, '.impeccable', 'live', 'server.json');
  let delayed;
  let stopper;
  try {
    assert.equal(fs.existsSync(recordPath), true);
    const info = JSON.parse(fs.readFileSync(recordPath, 'utf8'));
    const agentStateDir = path.dirname(info.agentStatePath);
    delayed = net.createConnection({ port: info.port, host: '127.0.0.1' });
    delayed.on('error', () => {});
    await new Promise((resolve, reject) => {
      delayed.once('connect', resolve);
      delayed.once('error', reject);
    });
    const continueReceived = new Promise((resolve, reject) => {
      let response = '';
      const onData = (chunk) => {
        response += chunk.toString();
        if (response.includes('HTTP/1.1 100 Continue')) {
          delayed.off('data', onData);
          resolve();
        }
      };
      delayed.on('data', onData);
      delayed.once('error', reject);
    });
    delayed.write(
      `POST /annotation?token=${info.token}&eventId=slow-upload HTTP/1.1\r\n`
      + 'Host: 127.0.0.1\r\n'
      + `Content-Type: image/png\r\n`
      + 'Content-Length: 100\r\n'
      + 'Expect: 100-continue\r\n'
      + 'Connection: close\r\n\r\n',
    );
    // Node acknowledges Expect: 100-continue only after it has accepted the
    // request headers, giving the regression a server-observed barrier before
    // the incomplete body is sent.
    await continueReceived;
    delayed.write('x');
    const uploadClosedByServer = new Promise((resolve) => {
      delayed.once('close', resolve);
    });

    stopper = spawn(process.execPath, [SERVER, 'stop', '--keep-inject'], {
      cwd: project,
      env: serverEnv(),
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const stopperExit = new Promise((resolve) => {
      stopper.once('exit', (code) => resolve(code));
    });
    // Keep the incomplete upload in flight. The server must close this socket
    // as part of shutdown; the client is not allowed to make that happen.
    await uploadClosedByServer;
    assert.equal(delayed.destroyed, true);
    const [stopStdout, stopStderr, stopCode] = await Promise.all([
      new Promise((resolve) => {
        let output = '';
        stopper.stdout.on('data', (chunk) => { output += chunk; });
        stopper.stdout.once('close', () => resolve(output));
      }),
      new Promise((resolve) => {
        let output = '';
        stopper.stderr.on('data', (chunk) => { output += chunk; });
        stopper.stderr.once('close', () => resolve(output));
      }),
      stopperExit,
    ]);
    assert.equal(stopCode, 0, stopStderr);
    assert.match(stopStdout, /Stopped live server on port/);
    assert.throws(() => process.kill(info.pid, 0), /ESRCH/);
    assert.equal(fs.existsSync(agentStateDir), false);
    assert.equal(fs.existsSync(recordPath), false);
  } finally {
    delayed?.destroy();
    if (stopper?.exitCode === null) stopper.kill('SIGTERM');
    if (stopper?.exitCode === null) await new Promise((resolve) => stopper.once('exit', resolve));
    if (fs.existsSync(recordPath)) stopServer(project);
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('direct stop reports cleanup failure and preserves the exact state record', async () => {
  const project = newProject();
  const launcher = spawnSync(process.execPath, [SERVER, '--background'], {
    cwd: project,
    env: serverEnv(),
    encoding: 'utf8',
    timeout: 15_000,
  });
  assert.equal(launcher.status, 0, launcher.stderr);
  const recordPath = path.join(project, '.impeccable', 'live', 'server.json');
  let stopper;
  let stateDir;
  try {
    const info = JSON.parse(fs.readFileSync(recordPath, 'utf8'));
    stateDir = path.dirname(info.agentStatePath);
    fs.chmodSync(stateDir, 0o500);
    stopper = spawn(process.execPath, [SERVER, 'stop', '--keep-inject'], {
      cwd: project,
      env: serverEnv(),
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stderr = '';
    stopper.stderr.on('data', (chunk) => { stderr += chunk; });
    const stopCode = await new Promise((resolve) => {
      stopper.once('exit', (code) => resolve(code));
    });
    assert.equal(stopCode, 1, stderr);
    const diagnostic = JSON.parse(stderr.trim());
    assert.equal(diagnostic.error, 'server_cleanup_incomplete');
    assert.equal(diagnostic.pid, info.pid);
    assert.equal(diagnostic.path, fs.realpathSync(recordPath));
    assert.equal(fs.existsSync(recordPath), true);
    assert.equal(fs.existsSync(stateDir), true);
  } finally {
    if (stopper?.exitCode === null) stopper.kill('SIGTERM');
    if (stopper?.exitCode === null) await new Promise((resolve) => stopper.once('exit', resolve));
    if (stateDir) {
      fs.chmodSync(stateDir, 0o700);
      fs.rmSync(stateDir, { recursive: true, force: true });
      assert.equal(fs.existsSync(stateDir), false);
    }
    if (fs.existsSync(recordPath)) stopServer(project);
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('direct stop preserves a live PID when its authenticated probe fails', async () => {
  const project = newProject();
  const unrelated = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], {
    stdio: 'ignore',
  });
  const recordPath = path.join(project, '.impeccable', 'live', 'server.json');
  try {
    fs.mkdirSync(path.dirname(recordPath), { recursive: true });
    fs.writeFileSync(recordPath, JSON.stringify({
      pid: unrelated.pid,
      port: 65534,
      token: 'test-token',
    }));
    const result = spawnSync(process.execPath, [SERVER, 'stop', '--keep-inject'], {
      cwd: project,
      env: serverEnv(),
      encoding: 'utf8',
      timeout: 5_000,
    });
    assert.equal(result.status, 1);
    const diagnostic = JSON.parse(result.stderr.trim());
    assert.equal(diagnostic.error, 'live_server_probe_failed');
    assert.equal(diagnostic.pid, unrelated.pid);
    assert.equal(fs.existsSync(recordPath), true);
    assert.doesNotThrow(() => process.kill(unrelated.pid, 0));
  } finally {
    if (unrelated.exitCode === null) unrelated.kill('SIGTERM');
    if (unrelated.exitCode === null) await new Promise((resolve) => unrelated.once('exit', resolve));
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('direct stop bounds a held stop request and reports its exact identity', async () => {
  const project = newProject();
  const childScript = `
    import http from 'node:http';
    import fs from 'node:fs';
    import path from 'node:path';
    const project = process.argv[1];
    const recordPath = path.join(project, '.impeccable', 'live', 'server.json');
    const server = http.createServer((req, res) => {
      if (req.url.startsWith('/status')) {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ status: 'ok', pid: process.pid, port: server.address().port }));
        return;
      }
      if (req.url.startsWith('/stop')) return;
      res.writeHead(404);
      res.end();
    });
    server.listen(0, '127.0.0.1', () => {
      fs.mkdirSync(path.dirname(recordPath), { recursive: true });
      fs.writeFileSync(recordPath, JSON.stringify({
        pid: process.pid, port: server.address().port, token: 'test-token',
      }));
    });
  `;
  const fake = spawn(process.execPath, ['--input-type=module', '-e', childScript, project], {
    stdio: 'ignore',
  });
  const recordPath = path.join(project, '.impeccable', 'live', 'server.json');
  let stopper;
  try {
    const deadline = Date.now() + 2_000;
    while (!fs.existsSync(recordPath) && fake.exitCode === null && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    assert.equal(fake.exitCode, null);
    const info = JSON.parse(fs.readFileSync(recordPath, 'utf8'));
    stopper = spawn(process.execPath, [SERVER, 'stop', '--keep-inject'], {
      cwd: project,
      env: serverEnv(),
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stderr = '';
    stopper.stderr.on('data', (chunk) => { stderr += chunk; });
    const stopCode = await new Promise((resolve) => stopper.once('exit', (code) => resolve(code)));
    assert.equal(stopCode, 1);
    const diagnostic = JSON.parse(stderr.trim());
    assert.equal(diagnostic.error, 'live_server_stop_failed');
    assert.equal(diagnostic.reason, 'stop_request_timeout');
    assert.equal(diagnostic.pid, info.pid);
    assert.equal(diagnostic.port, info.port);
    assert.equal(fs.existsSync(recordPath), true);
  } finally {
    if (stopper?.exitCode === null) stopper.kill('SIGTERM');
    if (stopper?.exitCode === null) await new Promise((resolve) => stopper.once('exit', resolve));
    if (fake.exitCode === null) fake.kill('SIGTERM');
    if (fake.exitCode === null) await new Promise((resolve) => fake.once('exit', resolve));
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('shutdown bounds record-lock contention and retains its record', async () => {
  const project = newProject();
  const server = spawn(process.execPath, [SERVER], {
    cwd: project,
    env: serverEnv(),
    stdio: 'ignore',
  });
  const recordPath = path.join(project, '.impeccable', 'live', 'server.json');
  const lockPath = `${recordPath}.lock`;
  try {
    const deadline = Date.now() + 2_000;
    while (!fs.existsSync(recordPath) && server.exitCode === null && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    assert.equal(server.exitCode, null);
    const info = JSON.parse(fs.readFileSync(recordPath, 'utf8'));
    fs.mkdirSync(lockPath);
    const response = await fetch(
      `http://127.0.0.1:${info.port}/stop?token=${encodeURIComponent(info.token)}`,
    );
    assert.equal(response.status, 200);
    const stopCode = await new Promise((resolve) => server.once('exit', resolve));
    assert.equal(stopCode, 0);
    assert.equal(fs.existsSync(recordPath), true);
  } finally {
    if (server.exitCode === null) server.kill('SIGKILL');
    if (server.exitCode === null) await new Promise((resolve) => server.once('exit', resolve));
    if (fs.existsSync(lockPath)) fs.rmdirSync(lockPath);
    if (fs.existsSync(recordPath)) stopServer(project);
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('direct stop resolves each pending poll once before final cleanup', async () => {
  const project = newProject();
  const launcher = spawnSync(process.execPath, [SERVER, '--background'], {
    cwd: project,
    env: serverEnv(),
    encoding: 'utf8',
    timeout: 15_000,
  });
  assert.equal(launcher.status, 0, launcher.stderr);
  const recordPath = path.join(project, '.impeccable', 'live', 'server.json');
  let stopper;
  try {
    const info = JSON.parse(fs.readFileSync(recordPath, 'utf8'));
    const agentToken = JSON.parse(fs.readFileSync(info.agentStatePath, 'utf8')).agentToken;
    const poll = fetch(
      `http://127.0.0.1:${info.port}/poll?token=${encodeURIComponent(agentToken)}&timeout=600000`,
    );
    await new Promise((resolve) => setTimeout(resolve, 50));
    stopper = spawn(process.execPath, [SERVER, 'stop', '--keep-inject'], {
      cwd: project,
      env: serverEnv(),
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    const response = await poll;
    assert.equal(response.status, 200);
    assert.deepEqual(await response.json(), { type: 'exit' });
    const stopCode = await new Promise((resolve) => stopper.once('exit', (code) => resolve(code)));
    assert.equal(stopCode, 0);
    assert.equal(fs.existsSync(recordPath), false);
  } finally {
    if (stopper?.exitCode === null) stopper.kill('SIGTERM');
    if (stopper?.exitCode === null) await new Promise((resolve) => stopper.once('exit', resolve));
    if (fs.existsSync(recordPath)) stopServer(project);
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('background launcher reports a successful startup without exposing its bearer token', () => {
  const project = newProject();
  try {
    const result = spawnSync(process.execPath, [SERVER, '--background'], {
      cwd: project,
      encoding: 'utf8',
      timeout: 15_000,
      env: serverEnv(),
    });

    assert.equal(result.status, 0, result.stderr);
    const info = JSON.parse(result.stdout);
    assert.equal(typeof info.pid, 'number');
    assert.equal(typeof info.port, 'number');
    assert.deepEqual(Object.keys(info).sort(), ['pid', 'port']);
    const privateInfo = JSON.parse(fs.readFileSync(
      path.join(project, '.impeccable', 'live', 'server.json'),
      'utf8',
    ));
    assert.equal(typeof privateInfo.token, 'string');
    assert.equal(result.stdout.includes(privateInfo.token), false);
    assert.equal(result.stderr.includes(privateInfo.token), false);
    assert.equal(result.stderr, '');
  } finally {
    stopServer(project);
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('background launcher replaces a stale server record before reporting success', () => {
  const project = newProject();
  const serverRecord = path.join(project, '.impeccable', 'live');
  fs.mkdirSync(serverRecord, { recursive: true });
  fs.writeFileSync(
    path.join(serverRecord, 'server.json'),
    JSON.stringify({ pid: 2_147_483_647, port: 8400, token: 'stale' }),
  );
  try {
    const result = spawnSync(process.execPath, [SERVER, '--background'], {
      cwd: project,
      encoding: 'utf8',
      timeout: 15_000,
      env: serverEnv(),
    });

    assert.equal(result.status, 0, result.stderr);
    const info = JSON.parse(result.stdout);
    assert.notEqual(info.pid, 2_147_483_647);
    assert.deepEqual(Object.keys(info).sort(), ['pid', 'port']);
    const privateInfo = JSON.parse(fs.readFileSync(
      path.join(project, '.impeccable', 'live', 'server.json'),
      'utf8',
    ));
    assert.notEqual(privateInfo.token, 'stale');
    assert.equal(result.stdout.includes(privateInfo.token), false);
    assert.equal(result.stderr.includes(privateInfo.token), false);
  } finally {
    stopServer(project);
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('background launcher replaces a stale record that reuses a live unrelated pid', () => {
  const project = newProject();
  const unrelated = spawn(process.execPath, ['-e', 'setInterval(()=>{},1000)'], {
    stdio: 'ignore',
  });
  const serverRecord = path.join(project, '.impeccable', 'live');
  fs.mkdirSync(serverRecord, { recursive: true });
  fs.writeFileSync(
    path.join(serverRecord, 'server.json'),
    JSON.stringify({ pid: unrelated.pid, port: 65534, token: 'stale' }),
  );
  try {
    const result = spawnSync(process.execPath, [SERVER, '--background'], {
      cwd: project,
      encoding: 'utf8',
      timeout: 15_000,
      env: serverEnv(),
    });

    assert.equal(result.status, 0, result.stderr);
    const info = JSON.parse(result.stdout);
    assert.notEqual(info.pid, unrelated.pid);
    assert.doesNotThrow(() => process.kill(unrelated.pid, 0));
  } finally {
    stopServer(project);
    unrelated.kill('SIGTERM');
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('foreground server reports a structured occupied-port bind failure', async () => {
  const project = newProject();
  const occupied = net.createServer();
  await new Promise((resolve) => occupied.listen(0, '127.0.0.1', resolve));
  const port = occupied.address().port;
  try {
    assert.equal(fs.existsSync(
      path.join(project, '.impeccable', 'live', 'server.json'),
    ), false);
    const result = spawnSync(process.execPath, [SERVER, `--port=${port}`], {
      cwd: project,
      encoding: 'utf8',
      timeout: 5_000,
      env: serverEnv(),
    });

    assert.notEqual(result.status, 0);
    const diagnostic = JSON.parse(result.stderr);
    assert.deepEqual(diagnostic, {
      error: 'live_server_bind_failed',
      code: 'EADDRINUSE',
      port,
      message: `Unable to bind live server to 127.0.0.1:${port}`,
    });
    assert.equal(fs.existsSync(
      path.join(project, '.impeccable', 'live', 'server.json'),
    ), false);
  } finally {
    await new Promise((resolve) => occupied.close(resolve));
    fs.rmSync(project, { recursive: true, force: true });
  }
});
