import fs from 'node:fs';
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

async function waitForServerInfo(project, child) {
  const infoPath = path.join(project, '.impeccable', 'live', 'server.json');
  const deadline = Date.now() + 5_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) throw new Error(`server exited with ${child.exitCode}`);
    try {
      return JSON.parse(fs.readFileSync(infoPath, 'utf8'));
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 25));
    }
  }
  throw new Error('timed out waiting for live server fixture');
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

test('background launcher reports a successful startup using its compatible JSON payload', () => {
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
    assert.equal(typeof info.token, 'string');
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
    assert.notEqual(info.token, 'stale');
  } finally {
    stopServer(project);
    fs.rmSync(project, { recursive: true, force: true });
  }
});

test('background launcher reports an occupied-port startup refusal with observation', async () => {
  const project = newProject();
  const server = spawn(process.execPath, [SERVER], {
    cwd: project,
    stdio: 'ignore',
  });
  try {
    const info = await waitForServerInfo(project, server);
    const result = spawnSync(process.execPath, [SERVER, '--background', `--port=${info.port}`], {
      cwd: project,
      encoding: 'utf8',
      timeout: 15_000,
    });

    assert.notEqual(result.status, 0);
    const diagnostic = JSON.parse(result.stderr);
    assert.equal(diagnostic.status, 'refused');
    assert.equal(diagnostic.observation.baselineMs, 10_000);
    assert.ok(diagnostic.observation.elapsedMs >= 0);
  } finally {
    stopServer(project);
    if (server.exitCode === null) server.kill('SIGTERM');
    fs.rmSync(project, { recursive: true, force: true });
  }
});
