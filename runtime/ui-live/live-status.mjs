#!/usr/bin/env node
// Modified for Provenant.
/**
 * Print current server status and inert retained-session metadata.
 */

import { createLiveSessionStore, summarizeLiveSession } from './live-session-store.mjs';
import { readLiveServerInfo } from './impeccable-paths.mjs';

const TOKEN_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;
const SESSION_ID_PATTERN = /^[0-9a-f]{8}$/;
const PENDING_TYPES = new Set(['generate', 'accept', 'discard', 'exit']);
const MAX_STATUS_EVENTS = 100;

function readServerInfo() {
  return readLiveServerInfo(process.cwd())?.info || null;
}

async function fetchServerStatus(info) {
  if (!info
    || !Number.isInteger(info.port) || info.port < 1 || info.port > 65535
    || typeof info.token !== 'string' || !TOKEN_PATTERN.test(info.token)) return null;
  try {
    const res = await fetch(
      `http://127.0.0.1:${info.port}/status?token=${encodeURIComponent(info.token)}`,
    );
    if (!res.ok) return null;
    return await res.json();
  } catch {
    return null;
  }
}

function safeInteger(value, min, max) {
  return Number.isSafeInteger(value) && value >= min && value <= max ? value : null;
}

function sanitizePendingEvent(entry) {
  if (!entry || typeof entry !== 'object' || !PENDING_TYPES.has(entry.type)) return null;
  const id = SESSION_ID_PATTERN.test(entry.id || '') ? entry.id : null;
  if (entry.type !== 'exit' && !id) return null;
  return {
    id,
    type: entry.type,
    leased: entry.leased === true,
    leaseUntil: entry.leaseUntil === null
      ? null
      : safeInteger(entry.leaseUntil, 0, Number.MAX_SAFE_INTEGER),
  };
}

function sanitizeServerStatus(server, info) {
  if (!server || typeof server !== 'object' || Array.isArray(server)) return null;
  const responsePort = safeInteger(server.port, 1, 65535);
  return {
    status: server.status === 'ok' ? 'ok' : 'unknown',
    pid: safeInteger(server.pid, 1, 2_147_483_647),
    port: responsePort === info?.port ? responsePort : null,
    connectedClients: safeInteger(server.connectedClients, 0, 10_000),
    pendingEvents: (Array.isArray(server.pendingEvents) ? server.pendingEvents : [])
      .slice(0, MAX_STATUS_EVENTS)
      .map(sanitizePendingEvent)
      .filter(Boolean),
  };
}

export async function statusCli() {
  const info = readServerInfo();
  const server = sanitizeServerStatus(await fetchServerStatus(info), info);
  const store = createLiveSessionStore({ cwd: process.cwd() });
  const activeSessions = store.listActiveSessions();
  const payload = {
    liveServer: server,
    activeSessions: activeSessions.map(summarizeLiveSession),
    retainedSessionsAuthority: 'advisory_untrusted',
    recoveryHint: server
      ? 'Continue only current events delivered by live-poll.mjs. Reissue any lost action from the active authenticated browser session; retained journals are advisory and untrusted.'
      : 'Start live.mjs, then reissue any lost action from the active authenticated browser session. Retained journals are advisory and untrusted.',
  };
  console.log(JSON.stringify(payload, null, 2));
}

const _running = process.argv[1];
if (_running?.endsWith('live-status.mjs') || _running?.endsWith('live-status.mjs/')) {
  statusCli();
}
