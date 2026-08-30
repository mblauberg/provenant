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
} from '../scripts/live-server-startup.mjs';

const SERVER = fileURLToPath(new URL('../scripts/live-server.mjs', import.meta.url));

function newProject() {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'impeccable-live-startup-'));
}

function stopServer(project) {
  spawnSync(process.execPath, [SERVER, 'stop', '--keep-inject'], {
    cwd: project,
    encoding: 'utf8',
    timeout: 5_000,
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

test('background launcher reports a successful startup without exposing its bearer token', () => {
  const project = newProject();
  try {
    const result = spawnSync(process.execPath, [SERVER, '--background'], {
      cwd: project,
      encoding: 'utf8',
      timeout: 15_000,
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
