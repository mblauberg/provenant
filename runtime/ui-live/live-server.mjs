#!/usr/bin/env node
// Modified for Provenant.
/**
 * Live variant mode server (self-contained, zero dependencies).
 *
 * Serves the browser script (/live.js), the detection overlay (/detect.js),
 * uses Server-Sent Events (SSE) for server→browser push, and HTTP POST for
 * browser→server events. Agent communicates via HTTP long-poll (/poll).
 *
 * Usage:
 *   node <scripts_path>/live-server.mjs              # start
 *   node <scripts_path>/live-server.mjs stop         # stop + remove injected live.js tag
 *   node <scripts_path>/live-server.mjs stop --keep-inject   # stop only
 *   node <scripts_path>/live-server.mjs --help
 */

import http from 'node:http';
import { randomUUID } from 'node:crypto';
import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import net from 'node:net';
import { fileURLToPath } from 'node:url';
import { parseDesignMd } from './design-parser.mjs';
import { loadContext, resolveContextDir } from './load-context.mjs';
import { resolveFiles } from './live-inject.mjs';
import { createLiveSessionStore, summarizeLiveSession } from './live-session-store.mjs';
import { readContainedSource } from './contained-source.mjs';
import {
  ensureCanonicalLiveStateRoot,
  getDesignSidecarPath,
  getLiveServerPath,
  readLiveServerInfo,
  resolveLiveConfigPath,
  resolveDesignSidecarPath,
  writeLiveServerInfo,
  writeLiveAgentServerInfo,
} from './impeccable-paths.mjs';
import {
  classifyStartupOutcome,
  LIVE_SERVER_STARTUP_TIMEOUT_MS,
  observeStartup,
} from './live-server-startup.mjs';
import {
  LIVE_SERVER_STOP_TIMEOUT_MS,
  isProcessAlive,
  waitForProcessExit,
} from './live-process.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const UI_EVIDENCE_BROWSER_BUNDLE = fileURLToPath(
  new URL('../ui-evidence/detector/detect-antipatterns-browser.js', import.meta.url),
);
// PRODUCT.md / DESIGN.md live wherever load-context.mjs resolves. The generated
// DESIGN sidecar is project-local at .impeccable/design.json, with legacy
// DESIGN.json fallback for existing projects.
const CONTEXT_DIR = resolveContextDir(process.cwd());
const DEFAULT_POLL_TIMEOUT = 600_000;   // 10 min — agent re-polls on timeout anyway
const DEFAULT_LEASE_MS = 600_000;
const MAX_POLL_TIMEOUT = 600_000;
const MAX_LEASE_MS = 600_000;
const SSE_HEARTBEAT_INTERVAL = 30_000;  // keepalive ping every 30s
const MAX_RECENT_TERMINAL_OUTCOMES = 128;
const SAFE_SOURCE_EXTENSIONS = new Set([
  '.astro', '.htm', '.html', '.jsx', '.svelte', '.tsx', '.vue',
]);

// ---------------------------------------------------------------------------
// Port detection
// ---------------------------------------------------------------------------

async function findOpenPort(start = 8400) {
  if (!Number.isInteger(start) || start < 1 || start > 65535) {
    throw new Error('No usable live server port remains');
  }
  return new Promise((resolve, reject) => {
    const srv = net.createServer();
    srv.listen(start, '127.0.0.1', () => {
      const port = srv.address().port;
      srv.close(() => resolve(port));
    });
    srv.on('error', (error) => {
      if (error?.code === 'EADDRINUSE' && start < 65535) {
        resolve(findOpenPort(start + 1));
      } else {
        reject(error);
      }
    });
  });
}

async function probeServerInfo(info, timeoutMs = 1_000) {
  if (!info
      || !Number.isInteger(info.pid)
      || !Number.isInteger(info.port)
      || info.port < 1
      || info.port > 65535
      || typeof info.token !== 'string'
      || !info.token) return false;
  try {
    const response = await fetch(
      `http://127.0.0.1:${info.port}/status?probe=1&token=${encodeURIComponent(info.token)}`,
      { signal: AbortSignal.timeout(timeoutMs) },
    );
    if (!response.ok) return false;
    const status = await response.json();
    return status?.status === 'ok'
      && status.pid === info.pid
      && status.port === info.port;
  } catch {
    return false;
  }
}

async function acquireServerRecordLock(timeoutMs = LIVE_SERVER_STARTUP_TIMEOUT_MS) {
  ensureCanonicalLiveStateRoot(process.cwd());
  const lockPath = getLiveServerPath(process.cwd()) + '.lock';
  fs.mkdirSync(path.dirname(lockPath), { recursive: true });
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      fs.mkdirSync(lockPath, { mode: 0o700 });
      ensureCanonicalLiveStateRoot(process.cwd());
      let released = false;
      return () => {
        if (released) return;
        released = true;
        try { fs.rmdirSync(lockPath); } catch {}
      };
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      try {
        if (Date.now() - fs.statSync(lockPath).mtimeMs > LIVE_SERVER_STARTUP_TIMEOUT_MS * 2) {
          fs.rmdirSync(lockPath);
          continue;
        }
      } catch (staleError) {
        if (!['ENOENT', 'ENOTEMPTY'].includes(staleError.code)) throw staleError;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  const error = new Error('Timed out waiting for live server state ownership');
  error.code = 'live_server_lock_timeout';
  throw error;
}

function removeServerRecordIfUnchanged(record) {
  if (!record?.path || !record.info) return false;
  const current = readLiveServerInfo(process.cwd());
  if (!current || current.path !== record.path) return false;
  for (const field of ['pid', 'port', 'token', 'agentStatePath']) {
    if ((current.info?.[field] ?? null) !== (record.info?.[field] ?? null)) return false;
  }
  try {
    fs.unlinkSync(record.path);
    return true;
  } catch {
    return false;
  }
}

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

const state = {
  token: null,
  agentToken: null,
  port: null,
  sseClients: new Set(),   // SSE response objects (server→browser push)
  pendingEvents: [],       // browser events waiting for agent ack ({ event, leaseUntil, leaseToken })
  seenBrowserEvents: new Set(),
  pendingPolls: [],        // agent poll callbacks waiting for browser events
  terminalOutcomes: [],    // bounded in-memory SSE replay for reconnecting browsers
  sseEventSequence: 0,
  exitTimer: null,
  sessionDir: null,         // per-session tmp dir for annotation screenshots
  activeUploads: new Set(),
  annotationFiles: new Map(),
  annotationBytes: 0,
  annotationUploads: 0,
  annotationInflightBytes: 0,
  agentStatePath: null,
  sessionStore: null,
  leaseTimer: null,
  releaseServerRecordLock: null,
};

// Cap per-annotation upload size. A full 1920×1080 PNG is typically <1 MB;
// cap at 10 MB to guard against runaway writes from a misbehaving client.
const MAX_ANNOTATION_BYTES = 10 * 1024 * 1024;
const MAX_ANNOTATION_FILES = 64;
const MAX_ANNOTATION_TOTAL_BYTES = 64 * 1024 * 1024;
const MAX_JSON_BODY_BYTES = 256 * 1024;
const MAX_PENDING_EVENTS = 64;
const MAX_ACTIVE_SESSIONS = 64;
const MAX_SSE_CLIENTS = 16;
const TERMINAL_SESSION_PHASES = new Set(['completed', 'discarded']);
const SESSION_CLEANUP_RETRIES = 4;
const SESSION_CLEANUP_RETRY_DELAY_MS = 25;
// Leave a small margin inside the caller's stop window for cleanup and the
// final exact-PID observation. This prevents shutdown lock contention from
// consuming the entire external stop budget.
const SHUTDOWN_LOCK_MARGIN_MS = 1_000;

function redactBearer(event) {
  if (!event || typeof event !== 'object') return event;
  const { token: _bearerToken, ...safeEvent } = event;
  return safeEvent;
}

function enqueueEvent(event) {
  const safeEvent = redactBearer(event);
  if (!safeEvent || isPendingEventDuplicate(safeEvent)) return;
  state.pendingEvents.push({ event: safeEvent, leaseUntil: 0, leaseToken: null });
  flushPendingPolls();
}

function isPendingEventDuplicate(event) {
  return Boolean(event?.id && state.pendingEvents.some((entry) => (
    entry.event?.id === event.id && entry.event?.type === event.type
  )));
}

function releaseAnnotationForEvent(event) {
  const tracked = event?.id ? state.annotationFiles.get(event.id) : null;
  if (!tracked) return;
  try {
    fs.unlinkSync(tracked.path);
  } catch (error) {
    if (error.code !== 'ENOENT') return;
  }
  state.annotationFiles.delete(event.id);
  state.annotationBytes = Math.max(0, state.annotationBytes - tracked.bytes);
}

function reclaimOldestUnboundAnnotation() {
  for (const [eventId, tracked] of state.annotationFiles) {
    if (tracked.bound) continue;
    releaseAnnotationForEvent({ id: eventId });
    return true;
  }
  return false;
}

function reclaimUnboundAnnotationsUntil(predicate) {
  while (predicate() && reclaimOldestUnboundAnnotation()) {
    // Each pass releases at most one tracked upload and updates byte totals.
  }
}

function findAvailablePendingEvent(now = Date.now()) {
  return state.pendingEvents.find((entry) => !entry.leaseUntil || entry.leaseUntil <= now);
}

function leaseEvent(entry, leaseMs) {
  if (!entry.event?.id) {
    const idx = state.pendingEvents.indexOf(entry);
    if (idx !== -1) state.pendingEvents.splice(idx, 1);
    return entry.event;
  }
  entry.leaseUntil = Date.now() + leaseMs;
  entry.leaseToken = randomUUID();
  return { ...entry.event, leaseToken: entry.leaseToken };
}

function findLeasedPendingEventIndex(id, leaseToken) {
  return state.pendingEvents.findIndex((entry) => (
    entry.event?.id === id
      && entry.leaseUntil > Date.now()
      && typeof leaseToken === 'string'
      && entry.leaseToken === leaseToken
  ));
}

function isCompatibleAgentReply(event, message) {
  if (!event) return false;
  if (message.type === 'error') return true;
  if (event.type === 'generate') return ['agent_done', 'done'].includes(message.type);
  if (event.type === 'accept') {
    return message.type === 'complete'
      || (message.type === 'agent_done' && message.data?.carbonize === true);
  }
  if (event.type === 'discard') return ['discard', 'discarded'].includes(message.type);
  return false;
}

function acknowledgePendingEvent(index, id, leaseToken) {
  const entry = state.pendingEvents[index];
  if (entry?.event?.id !== id || entry.leaseToken !== leaseToken) return false;
  state.pendingEvents.splice(index, 1);
  releaseAnnotationForEvent(entry.event);
  scheduleLeaseFlush();
  return true;
}

function scheduleLeaseFlush() {
  if (state.leaseTimer) {
    clearTimeout(state.leaseTimer);
    state.leaseTimer = null;
  }
  if (state.pendingPolls.length === 0) return;
  const now = Date.now();
  const nextLeaseUntil = state.pendingEvents
    .map((entry) => entry.leaseUntil || 0)
    .filter((leaseUntil) => leaseUntil > now)
    .sort((a, b) => a - b)[0];
  if (!nextLeaseUntil) return;
  state.leaseTimer = setTimeout(() => {
    state.leaseTimer = null;
    flushPendingPolls();
  }, Math.max(0, nextLeaseUntil - now));
}

function flushPendingPolls() {
  while (state.pendingPolls.length > 0) {
    const entry = findAvailablePendingEvent();
    if (!entry) {
      scheduleLeaseFlush();
      return;
    }
    const poll = state.pendingPolls.shift();
    poll.resolve(leaseEvent(entry, poll.leaseMs));
  }
  scheduleLeaseFlush();
}

function formatSseMessage(msg, eventId = null) {
  const id = eventId === null ? '' : `id: ${eventId}\n`;
  return id + 'data: ' + JSON.stringify(msg) + '\n\n';
}

/** Push a message to all connected SSE clients. */
function broadcast(msg, eventId = null) {
  const data = formatSseMessage(msg, eventId);
  for (const res of state.sseClients) {
    try { res.write(data); } catch { /* client gone */ }
  }
}

function rememberTerminalOutcome(msg) {
  if (!msg?.id) return null;
  const eventId = ++state.sseEventSequence;
  state.terminalOutcomes.push({ eventId, msg });
  if (state.terminalOutcomes.length > MAX_RECENT_TERMINAL_OUTCOMES) {
    state.terminalOutcomes.shift();
  }
  return eventId;
}

function parseLastSseEventId(req) {
  const raw = req.headers['last-event-id'];
  if (typeof raw !== 'string' || !/^[0-9]+$/.test(raw)) return null;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value > state.sseEventSequence) return null;
  return value;
}

// ---------------------------------------------------------------------------
// Load scripts
// ---------------------------------------------------------------------------

function loadBrowserScripts() {
  // The detector is product runtime, not target-project state. This one is
  // cached because detect.js rarely changes during a session.
  const detectScript = fs.readFileSync(UI_EVIDENCE_BROWSER_BUNDLE, 'utf-8');

  // live-browser.js: DO NOT cache. Return the path so the /live.js handler
  // can re-read on every request. Editing the browser script during iteration
  // should land on the next tab reload, not require a server restart.
  const sessionPath = path.join(__dirname, 'live-browser-session.js');
  const livePath = path.join(__dirname, 'live-browser.js');
  for (const p of [sessionPath, livePath]) {
    if (!fs.existsSync(p)) {
      process.stderr.write('Error: live browser script not found at ' + p + '\n');
      process.exit(1);
    }
  }

  return { detectScript, sessionPath, livePath };
}

function hasProjectContext() {
  // Keep the live signal identical to the shared loader, including legacy
  // `.impeccable.md`, fallback directories and unusual filename case.
  return loadContext(process.cwd()).hasProduct;
}

function pathExistsWithoutFollowing(filePath) {
  try { fs.lstatSync(filePath); return true; } catch { return false; }
}

function readAuthorisedDesignSidecar(filePath) {
  const candidate = path.resolve(filePath);
  const projectRoot = path.resolve(process.cwd());
  const contextRoot = path.resolve(CONTEXT_DIR);
  if (isContainedPath(projectRoot, candidate)) {
    return readContainedSource(projectRoot, candidate);
  }
  if (isContainedPath(contextRoot, candidate)) {
    return readContainedSource(contextRoot, candidate);
  }
  const error = new Error('Design sidecar is outside authorised roots');
  error.code = 'source_path_outside_project';
  throw error;
}

// ---------------------------------------------------------------------------
// Validation (inline — no external import needed for self-contained script)
// ---------------------------------------------------------------------------

const VISUAL_ACTIONS = [
  'impeccable', 'bolder', 'quieter', 'distill', 'polish', 'typeset',
  'colorize', 'layout', 'adapt', 'animate', 'delight', 'overdrive',
];

// Browser generates ids via crypto.randomUUID().slice(0, 8) (8 hex chars)
// and variantIds via String(small integer). Restrict to those shapes so
// any value that reaches a downstream child_process or DOM selector is
// inert by construction.
const ID_PATTERN = /^[0-9a-f]{8}$/;
const VARIANT_ID_PATTERN = /^[1-8]$/;

function isValidId(v) { return typeof v === 'string' && ID_PATTERN.test(v); }
function isValidVariantId(v) { return typeof v === 'string' && VARIANT_ID_PATTERN.test(v); }
function isJsonObject(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function validateEvent(msg) {
  if (!msg || typeof msg !== 'object' || !msg.type) return 'Missing or invalid message';
  switch (msg.type) {
    case 'generate':
      if (!isValidId(msg.id)) return 'generate: missing or malformed id';
      if (!msg.action || !VISUAL_ACTIONS.includes(msg.action)) return 'generate: invalid action';
      if (!Number.isInteger(msg.count) || msg.count < 1 || msg.count > 8) return 'generate: count must be 1-8';
      if (!msg.element || !msg.element.outerHTML) return 'generate: missing element context';
      // Optional annotation fields (all-or-nothing: if any present, all must be well-formed).
      if (msg.screenshotPath !== undefined && typeof msg.screenshotPath !== 'string') return 'generate: screenshotPath must be string';
      if (msg.comments !== undefined && !Array.isArray(msg.comments)) return 'generate: comments must be array';
      if (msg.strokes !== undefined && !Array.isArray(msg.strokes)) return 'generate: strokes must be array';
      return null;
    case 'accept':
      if (!isValidId(msg.id)) return 'accept: missing or malformed id';
      if (!isValidVariantId(msg.variantId)) return 'accept: missing or malformed variantId';
      if (msg.paramValues !== undefined) {
        if (typeof msg.paramValues !== 'object' || msg.paramValues === null || Array.isArray(msg.paramValues)) {
          return 'accept: paramValues must be an object';
        }
      }
      return null;
    case 'discard':
      return isValidId(msg.id) ? null : 'discard: missing or malformed id';
    case 'checkpoint':
      if (!isValidId(msg.id)) return 'checkpoint: missing or malformed id';
      if (!Number.isInteger(msg.revision) || msg.revision < 0) return 'checkpoint: revision must be a non-negative integer';
      if (msg.paramValues !== undefined && (typeof msg.paramValues !== 'object' || msg.paramValues === null || Array.isArray(msg.paramValues))) {
        return 'checkpoint: paramValues must be an object';
      }
      return null;
    case 'exit':
      return null;
    case 'prefetch':
      if (!msg.pageUrl || typeof msg.pageUrl !== 'string') return 'prefetch: missing pageUrl';
      return null;
    default:
      return 'Unknown event type: ' + msg.type;
  }
}

function bindUploadedScreenshot(msg) {
  if (msg.type !== 'generate' || msg.screenshotPath === undefined) return msg;
  if (!state.sessionDir) throw new Error('Session dir unavailable');
  const expected = path.join(state.sessionDir, `${msg.id}.png`);
  if (path.resolve(msg.screenshotPath) !== path.resolve(expected)) {
    throw new Error('generate: screenshotPath is not bound to this event upload');
  }
  const stat = fs.lstatSync(expected);
  if (!stat.isFile() || stat.isSymbolicLink() || stat.nlink !== 1) {
    throw new Error('generate: screenshot upload is not a private regular file');
  }
  return { ...msg, screenshotPath: expected };
}

function markUploadedScreenshotBound(event) {
  if (event?.type !== 'generate' || !event.screenshotPath) return;
  const tracked = state.annotationFiles.get(event.id);
  if (tracked) tracked.bound = true;
}

function readBoundedJsonBody(req, res, onMessage) {
  const declaredLength = req.headers['content-length'];
  if (declaredLength !== undefined) {
    const parsedLength = Number(declaredLength);
    if (!Number.isSafeInteger(parsedLength) || parsedLength < 0) {
      res.writeHead(400, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Invalid Content-Length' }));
      req.resume();
      return;
    }
    if (parsedLength > MAX_JSON_BODY_BYTES) {
      res.writeHead(413, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Payload too large' }));
      // Drain rather than destroy the socket so the deliberate 413 does not
      // become a client-side connection reset on a keep-alive connection.
      req.resume();
      return;
    }
  }

  const chunks = [];
  let total = 0;
  let settled = false;
  req.on('data', (chunk) => {
    if (settled) return;
    total += chunk.length;
    if (total > MAX_JSON_BODY_BYTES) {
      settled = true;
      chunks.length = 0;
      res.writeHead(413, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Payload too large' }));
      req.resume();
      return;
    }
    chunks.push(chunk);
  });
  req.on('end', () => {
    if (settled) return;
    settled = true;
    let message;
    try {
      message = JSON.parse(Buffer.concat(chunks, total).toString('utf-8'));
    } catch {
      res.writeHead(400, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Invalid JSON' }));
      return;
    }
    onMessage(message);
  });
  req.on('error', () => {
    if (settled || res.writableEnded) return;
    settled = true;
    res.writeHead(400, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: 'Request body failed' }));
  });
}

// ---------------------------------------------------------------------------
// HTTP request handler
// ---------------------------------------------------------------------------

function applyCors(req, res) {
  const origin = req.headers.origin;
  if (!origin) return;
  // Reflect the requesting project origin only on CORS-authorised responses.
  // Bearer-protected handlers call this after authentication; public
  // preflight contains no session data. Never publish a wildcard origin.
  res.setHeader('Access-Control-Allow-Origin', origin);
  res.setHeader('Vary', 'Origin');
  res.setHeader('Access-Control-Allow-Methods', 'GET, POST, OPTIONS');
  res.setHeader('Access-Control-Allow-Headers', 'Content-Type');
}

function authenticateQuery(req, res, url) {
  if (url.searchParams.get('token') !== state.token) {
    res.writeHead(401);
    res.end('Unauthorized');
    return false;
  }
  applyCors(req, res);
  return true;
}

function isContainedPath(rootPath, candidatePath) {
  const rel = path.relative(rootPath, candidatePath);
  return rel === '' || (!path.isAbsolute(rel) && rel !== '..' && !rel.startsWith(`..${path.sep}`));
}

function resolveProjectSource(rootDir, requestedPath) {
  if (!requestedPath || requestedPath.includes('\0') || path.isAbsolute(requestedPath)
      || path.win32.isAbsolute(requestedPath)) {
    const error = new Error('Bad path');
    error.code = 'BAD_PATH';
    throw error;
  }
  const rootReal = fs.realpathSync.native(path.resolve(rootDir));
  const lexicalPath = path.resolve(rootReal, requestedPath);
  if (!isContainedPath(rootReal, lexicalPath)) {
    const error = new Error('Forbidden');
    error.code = 'OUTSIDE_ROOT';
    throw error;
  }

  let sourceReal;
  try {
    sourceReal = fs.realpathSync.native(lexicalPath);
  } catch (cause) {
    const error = new Error('File not found', { cause });
    error.code = 'NOT_FOUND';
    throw error;
  }
  if (!isContainedPath(rootReal, sourceReal)) {
    const error = new Error('Forbidden');
    error.code = 'OUTSIDE_ROOT';
    throw error;
  }
  const stat = fs.statSync(sourceReal);
  if (!stat.isFile()) {
    const error = new Error('File not found');
    error.code = 'NOT_FOUND';
    throw error;
  }
  return sourceReal;
}

function resolveConfiguredProjectSource(rootDir, requestedPath) {
  // Validate containment and existence before consulting the allowlist so
  // malformed absolute/traversal paths retain their explicit 400/403 result.
  // This resolves metadata only; no source content is read here.
  const sourceReal = resolveProjectSource(rootDir, requestedPath);
  const portablePath = typeof requestedPath === 'string'
    ? requestedPath.split(path.sep).join('/')
    : '';
  if (!SAFE_SOURCE_EXTENSIONS.has(path.extname(portablePath).toLowerCase())) {
    const error = new Error('Forbidden');
    error.code = 'NOT_CONFIGURED_SOURCE';
    throw error;
  }

  const configPath = resolveLiveConfigPath({ cwd: rootDir });
  let config;
  try {
    config = JSON.parse(fs.readFileSync(configPath, 'utf-8'));
  } catch (cause) {
    const error = new Error('Live config unavailable', { cause });
    error.code = 'CONFIG_UNAVAILABLE';
    throw error;
  }
  if (!Array.isArray(config.files)) {
    const error = new Error('Live config unavailable');
    error.code = 'CONFIG_UNAVAILABLE';
    throw error;
  }

  let configuredFiles;
  try {
    configuredFiles = new Set(resolveFiles(rootDir, config).map((file) => file.split(path.sep).join('/')));
  } catch (cause) {
    const error = new Error('Live config unavailable', { cause });
    error.code = 'CONFIG_UNAVAILABLE';
    throw error;
  }
  if (!configuredFiles.has(portablePath)) {
    const error = new Error('Forbidden');
    error.code = 'NOT_CONFIGURED_SOURCE';
    throw error;
  }

  const rootReal = fs.realpathSync.native(path.resolve(rootDir));
  const lexicalPath = path.resolve(rootReal, requestedPath);
  // The endpoint returns source text. Even an in-project symlink can disguise
  // a secret file behind an allowlisted extension, so configured targets must
  // resolve to themselves rather than through a link.
  if (sourceReal !== lexicalPath) {
    const error = new Error('Forbidden');
    error.code = 'SYMLINK_SOURCE';
    throw error;
  }
  if (!SAFE_SOURCE_EXTENSIONS.has(path.extname(sourceReal).toLowerCase())) {
    const error = new Error('Forbidden');
    error.code = 'NOT_CONFIGURED_SOURCE';
    throw error;
  }
  // Bind the response bytes to a no-follow descriptor after every allowlist
  // check. Later pathname or parent swaps cannot redirect this read.
  return readContainedSource(rootDir, requestedPath, { relativeOnly: true });
}

function createRequestHandler({ detectScript, sessionPath, livePath }) {
  return (req, res) => {
    const url = new URL(req.url, `http://127.0.0.1:${state.port}`);
    const p = url.pathname;
    if (req.method === 'OPTIONS') {
      applyCors(req, res);
      res.writeHead(204);
      res.end();
      return;
    }

    // Once shutdown starts, reject new work while allowing requests already
    // reading a bounded body to drain and finish safely before close.
    if (shuttingDown && p !== '/stop') {
      res.writeHead(503, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Live server is stopping' }));
      req.resume();
      return;
    }

    // --- Scripts ---
    if (p === '/live.js') {
      // The script URL carries the bearer token. Refuse unauthenticated loads
      // and do not echo that secret into a CORS-readable response body.
      if (url.searchParams.get('token') !== state.token) {
        res.writeHead(401, { 'Content-Type': 'text/plain' });
        res.end('Unauthorized');
        return;
      }
      // Re-read from disk each request so edits to live-browser.js land on
      // the next tab reload. No-store headers prevent browser caching across
      // sessions — during iteration, a cached old script silently breaks
      // every subsequent session.
      let sessionScript;
      let liveScript;
      try {
        sessionScript = fs.readFileSync(sessionPath, 'utf-8');
        liveScript = fs.readFileSync(livePath, 'utf-8');
      } catch (err) {
        res.writeHead(500, { 'Content-Type': 'text/plain' });
        res.end('Error reading live browser scripts: ' + err.message);
        return;
      }
      const body =
        "window.__IMPECCABLE_TOKEN__ = new URL(document.currentScript.src).searchParams.get('token');\n" +
        `window.__IMPECCABLE_PORT__ = ${state.port};\n` +
        sessionScript + '\n' +
        liveScript;
      res.writeHead(200, {
        'Content-Type': 'application/javascript',
        'Cache-Control': 'no-store, no-cache, must-revalidate, max-age=0',
        'Pragma': 'no-cache',
      });
      res.end(body);
      return;
    }
    if (p === '/detect.js' || p === '/') {
      if (!detectScript) { res.writeHead(404); res.end('Not available'); return; }
      res.writeHead(200, { 'Content-Type': 'application/javascript' });
      res.end(detectScript);
      return;
    }

    // --- Vendored modern-screenshot (UMD build) ---
    // Lazy-loaded by live.js when the user clicks Go; exposes
    // window.modernScreenshot.domToBlob(...) for capture.
    if (p === '/modern-screenshot.js') {
      const vendorPath = path.join(__dirname, 'modern-screenshot.umd.js');
      try {
        res.writeHead(200, {
          'Content-Type': 'application/javascript',
          'Cache-Control': 'public, max-age=31536000, immutable',
        });
        res.end(fs.readFileSync(vendorPath));
      } catch {
        res.writeHead(404); res.end('Vendor script not found');
      }
      return;
    }

    // --- Annotation upload (browser → server, raw PNG body) ---
    // Client generates the eventId, POSTs the PNG, then POSTs the generate
    // event with screenshotPath already set. Keeps bytes out of the SSE/poll
    // bridge and preserves the "one shot from the user's POV" UX.
    if (p === '/annotation' && req.method === 'POST') {
      if (!authenticateQuery(req, res, url)) return;
      const eventId = url.searchParams.get('eventId');
      if (!eventId || !/^[A-Za-z0-9_-]{1,64}$/.test(eventId)) {
        res.writeHead(400, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'Invalid eventId' }));
        return;
      }
      if ((req.headers['content-type'] || '').toLowerCase() !== 'image/png') {
        res.writeHead(415, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'Content-Type must be image/png' }));
        return;
      }
      if (!state.sessionDir) {
        res.writeHead(500, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'Session dir unavailable' }));
        return;
      }
      if (state.annotationFiles.has(eventId)) {
        res.writeHead(409, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'Annotation already uploaded' }));
        return;
      }
      if (state.annotationFiles.size + state.annotationUploads >= MAX_ANNOTATION_FILES) {
        reclaimUnboundAnnotationsUntil(
          () => state.annotationFiles.size + state.annotationUploads >= MAX_ANNOTATION_FILES,
        );
      }
      if (state.annotationFiles.size + state.annotationUploads >= MAX_ANNOTATION_FILES) {
        res.writeHead(507, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'Annotation capacity reached' }));
        return;
      }
      state.annotationUploads += 1;
      state.activeUploads.add(req);
      const chunks = [];
      let total = 0;
      let aborted = false;
      let inflightReleased = false;
      const releaseInflight = () => {
        if (inflightReleased) return;
        inflightReleased = true;
        state.annotationUploads = Math.max(0, state.annotationUploads - 1);
        state.annotationInflightBytes = Math.max(0, state.annotationInflightBytes - total);
      };
      const releaseUpload = () => state.activeUploads.delete(req);
      req.on('data', (c) => {
        if (aborted) return;
        total += c.length;
        state.annotationInflightBytes += c.length;
        if (total > MAX_ANNOTATION_BYTES) {
          aborted = true;
          releaseInflight();
          res.writeHead(413, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Payload too large' }));
          req.destroy();
          return;
        }
        reclaimUnboundAnnotationsUntil(
          () => state.annotationBytes + state.annotationInflightBytes > MAX_ANNOTATION_TOTAL_BYTES,
        );
        if (state.annotationBytes + state.annotationInflightBytes > MAX_ANNOTATION_TOTAL_BYTES) {
          aborted = true;
          releaseInflight();
          res.writeHead(507, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Annotation capacity reached' }));
          req.destroy();
          return;
        }
        chunks.push(c);
      });
      req.on('end', () => {
        releaseUpload();
        if (aborted) return;
        releaseInflight();
        if (shuttingDown) {
          aborted = true;
          res.writeHead(503, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Live server is stopping' }));
          return;
        }
        reclaimUnboundAnnotationsUntil(
          () => state.annotationFiles.size >= MAX_ANNOTATION_FILES
            || state.annotationBytes + total > MAX_ANNOTATION_TOTAL_BYTES,
        );
        if (state.annotationFiles.size >= MAX_ANNOTATION_FILES
          || state.annotationBytes + total > MAX_ANNOTATION_TOTAL_BYTES) {
          res.writeHead(507, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Annotation capacity reached' }));
          return;
        }
        const absPath = path.join(state.sessionDir, eventId + '.png');
        try {
          fs.writeFileSync(absPath, Buffer.concat(chunks), { flag: 'wx', mode: 0o600 });
          fs.chmodSync(absPath, 0o600);
        } catch (err) {
          res.writeHead(err.code === 'EEXIST' ? 409 : 500, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Write failed: ' + err.message }));
          return;
        }
        state.annotationFiles.set(eventId, {
          path: absPath,
          bytes: total,
          bound: false,
        });
        state.annotationBytes += total;
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: true, path: absPath }));
      });
      req.on('error', () => {
        releaseUpload();
        releaseInflight();
        if (!aborted) {
          res.writeHead(500, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Upload failed' }));
        }
      });
      req.on('aborted', () => {
        releaseUpload();
        aborted = true;
        releaseInflight();
      });
      req.on('close', () => {
        releaseUpload();
        if (req.complete) return;
        aborted = true;
        releaseInflight();
      });
      return;
    }

    // --- Health ---
    if (p === '/status') {
      if (!authenticateQuery(req, res, url)) return;
      if (url.searchParams.get('probe') === '1') {
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ status: 'ok', pid: process.pid, port: state.port }));
        return;
      }
      const sessions = state.sessionStore
        ? state.sessionStore.listActiveSessions().map(summarizeLiveSession)
        : [];
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({
        status: 'ok',
        pid: process.pid,
        port: state.port,
        connectedClients: state.sseClients.size,
        pendingEvents: state.pendingEvents.map((entry) => ({
          id: entry.event?.id,
          type: entry.event?.type,
          leased: !!(entry.leaseUntil && entry.leaseUntil > Date.now()),
          leaseUntil: entry.leaseUntil || null,
        })),
        activeSessions: sessions,
      }));
      return;
    }

    if (p === '/health') {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({
        status: 'ok', pid: process.pid, port: state.port, mode: 'variant',
        hasProjectContext: hasProjectContext(),
        connectedClients: state.sseClients.size,
      }));
      return;
    }

    // --- Design system (unified v2 response) + raw ---
    //   /design-system.json    returns both parsed DESIGN.md and .impeccable/design.json
    //                          sidecar when present. Panel merges them:
    //                            { present, parsed, sidecar, hasMd, hasSidecar,
    //                              mdNewerThanJson, parseError?, sidecarError? }
    //                          - parsed: output of parseDesignMd (frontmatter
    //                            + six canonical sections) when DESIGN.md exists.
    //                          - sidecar: .impeccable/design.json contents when present.
    //                            Expected shape: schemaVersion 2, carrying
    //                            extensions + components + narrative.
    //   /design-system/raw     returns DESIGN.md markdown verbatim
    if (p === '/design-system.json' || p === '/design-system/raw') {
      if (!authenticateQuery(req, res, url)) return;

      const mdPath = path.join(CONTEXT_DIR, 'DESIGN.md');
      const existingJsonPath = resolveDesignSidecarPath(process.cwd(), CONTEXT_DIR);
      const jsonPath = existingJsonPath || getDesignSidecarPath(process.cwd());
      let mdSnapshot = null;
      let mdReadError = null;
      if (pathExistsWithoutFollowing(mdPath)) {
        try {
          mdSnapshot = readContainedSource(CONTEXT_DIR, mdPath);
        } catch (error) {
          mdReadError = error;
        }
      }
      let jsonSnapshot = null;
      let jsonReadError = null;
      if (existingJsonPath) {
        try {
          jsonSnapshot = readAuthorisedDesignSidecar(jsonPath);
        } catch (error) {
          jsonReadError = error;
        }
      }
      const jsonMtimeMs = jsonSnapshot ? Number(jsonSnapshot.mtimeNs) / 1_000_000 : null;

      if (p === '/design-system/raw') {
        if (mdReadError) { res.writeHead(403); res.end('Refused unsafe design markdown'); return; }
        if (!mdSnapshot) { res.writeHead(404); res.end('Not found'); return; }
        res.writeHead(200, { 'Content-Type': 'text/markdown; charset=utf-8' });
        res.end(mdSnapshot.bytes.toString('utf-8'));
        return;
      }

      if (!mdSnapshot && !mdReadError && !jsonSnapshot && !jsonReadError) {
        res.writeHead(404, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ present: false }));
        return;
      }

      const response = {
        present: true,
        hasMd: !!mdSnapshot,
        hasSidecar: !!jsonSnapshot,
        mdNewerThanJson: !!(mdSnapshot && jsonSnapshot
          && Number(mdSnapshot.mtimeNs) / 1_000_000 > jsonMtimeMs + 1000),
      };

      if (mdSnapshot) {
        try {
          response.parsed = parseDesignMd(mdSnapshot.bytes.toString('utf-8'));
        } catch (err) {
          response.parseError = err.message;
        }
      } else if (mdReadError) {
        response.parseError = 'Refused unsafe design markdown: ' + (mdReadError.code || 'read_failed');
      }

      if (jsonSnapshot) {
        try {
          response.sidecar = JSON.parse(jsonSnapshot.bytes.toString('utf-8'));
        } catch (err) {
          response.sidecarError = 'Failed to parse .impeccable/design.json: ' + err.message;
        }
      } else if (jsonReadError) {
        response.sidecarError = 'Refused unsafe design sidecar: ' + (jsonReadError.code || 'read_failed');
      }

      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(response));
      return;
    }

    // --- Source file (no-HMR fallback) ---
    if (p === '/source') {
      if (!authenticateQuery(req, res, url)) return;
      const filePath = url.searchParams.get('path');
      let sourceSnapshot;
      try {
        sourceSnapshot = resolveConfiguredProjectSource(process.cwd(), filePath);
      } catch (err) {
        const status = err.code === 'BAD_PATH'
          ? 400
          : err.code === 'CONFIG_UNAVAILABLE'
            ? 503
            : err.code === 'NOT_FOUND'
              ? 404
              : 403;
        res.writeHead(status);
        res.end(err.message);
        return;
      }
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      res.end(sourceSnapshot.bytes.toString('utf-8'));
      return;
    }

    // --- SSE: server→browser push (replaces WebSocket) ---
    if (p === '/events' && req.method === 'GET') {
      if (!authenticateQuery(req, res, url)) return;
      if (state.sseClients.size >= MAX_SSE_CLIENTS) {
        res.writeHead(429, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'SSE client capacity reached' }));
        return;
      }
      res.writeHead(200, {
        'Content-Type': 'text/event-stream',
        'Cache-Control': 'no-cache',
        'Connection': 'keep-alive',
      });
      const connected = {
        type: 'connected',
        hasProjectContext: hasProjectContext(),
      };
      const lastEventId = parseLastSseEventId(req);
      const replayFloor = state.terminalOutcomes[0]?.eventId ?? null;
      const replayGap = lastEventId !== null
        && replayFloor !== null
        && lastEventId < replayFloor - 1;
      res.write(formatSseMessage(
        connected,
        lastEventId === null ? state.sseEventSequence : null,
      ));
      if (lastEventId !== null) {
        for (const outcome of state.terminalOutcomes) {
          if (outcome.eventId > lastEventId) {
            res.write(formatSseMessage(outcome.msg, outcome.eventId));
          }
        }
      }
      if (replayGap) res.write(formatSseMessage({ type: 'replay_gap' }));

      state.sseClients.add(res);
      clearTimeout(state.exitTimer);

      // Keepalive: SSE comment every 30s prevents silent connection drops.
      const heartbeat = setInterval(() => {
        try { res.write(': keepalive\n\n'); } catch { clearInterval(heartbeat); }
      }, SSE_HEARTBEAT_INTERVAL);

      req.on('close', () => {
        clearInterval(heartbeat);
        state.sseClients.delete(res);
        if (!shuttingDown && state.sseClients.size === 0) {
          clearTimeout(state.exitTimer);
          state.exitTimer = setTimeout(() => {
            if (state.sseClients.size === 0) enqueueEvent({ type: 'exit' });
          }, 8000);
        }
      });
      return;
    }

    // --- Browser→server events (replaces WebSocket messages) ---
    if (p === '/events' && req.method === 'POST') {
      readBoundedJsonBody(req, res, (msg) => {
        if (shuttingDown) {
          res.writeHead(503, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Live server is stopping' }));
          return;
        }
        if (!isJsonObject(msg)) {
          res.writeHead(400, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Invalid message' }));
          return;
        }
        if (msg.token !== state.token) {
          res.writeHead(401, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Unauthorized' }));
          return;
        }
        applyCors(req, res);
        const error = validateEvent(msg);
        if (error) {
          res.writeHead(400, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error }));
          return;
        }
        const safeInput = redactBearer(msg);
        const browserEventKey = safeInput.id && ['generate', 'accept', 'discard'].includes(safeInput.type)
          ? `${safeInput.id}:${safeInput.type}`
          : null;
        if (browserEventKey && safeInput.retry !== true
          && state.seenBrowserEvents.has(browserEventKey)) {
          res.writeHead(200, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ ok: true, duplicate: true }));
          return;
        }
        let boundMessage;
        try {
          boundMessage = bindUploadedScreenshot(msg);
        } catch (err) {
          res.writeHead(400, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: err.message }));
          return;
        }
        const safeMessage = redactBearer(boundMessage);
        const duplicate = safeMessage.type !== 'checkpoint' && isPendingEventDuplicate(safeMessage);
        if (!duplicate && safeMessage.type !== 'checkpoint' && state.pendingEvents.length >= MAX_PENDING_EVENTS) {
          res.writeHead(429, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Pending event capacity reached' }));
          return;
        }
        if (state.sessionStore && safeMessage.id) {
          try {
            const existing = state.sessionStore.getSnapshot(safeMessage.id, {
              includeCompleted: true,
              requireExisting: true,
            });
            if (existing && TERMINAL_SESSION_PHASES.has(existing.phase)) {
              res.writeHead(409, { 'Content-Type': 'application/json' });
              res.end(JSON.stringify({ error: 'Session is terminal' }));
              return;
            }
            if (!existing
              && state.sessionStore.listActiveSessions().length >= MAX_ACTIVE_SESSIONS) {
              res.writeHead(429, { 'Content-Type': 'application/json' });
              res.end(JSON.stringify({ error: 'Active session capacity reached' }));
              return;
            }
            if (!duplicate) state.sessionStore.appendEvent(safeMessage);
          } catch (err) {
            res.writeHead(err.code === 'live_session_limit' ? 507 : 500, { 'Content-Type': 'application/json' });
            res.end(JSON.stringify({ error: 'session_store_append_failed', message: err.message }));
            return;
          }
        }
        if (!duplicate && safeMessage.type !== 'checkpoint') enqueueEvent(safeMessage);
        if (!duplicate) markUploadedScreenshotBound(safeMessage);
        if (browserEventKey) {
          state.seenBrowserEvents.add(browserEventKey);
          if (state.seenBrowserEvents.size > MAX_RECENT_TERMINAL_OUTCOMES) {
            state.seenBrowserEvents.delete(state.seenBrowserEvents.values().next().value);
          }
        }
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: true }));
      });
      return;
    }

    // --- Stop ---
    if (p === '/stop') {
      if (!authenticateQuery(req, res, url)) return;
      res.writeHead(200, { 'Content-Type': 'text/plain' });
      res.end('stopping');
      setImmediate(() => void shutdown());
      return;
    }

    // --- Agent poll ---
    if (p === '/poll' && req.method === 'GET') {
      handlePollGet(req, res, url);
      return;
    }
    if (p === '/poll' && req.method === 'POST') {
      handlePollPost(req, res);
      return;
    }

    res.writeHead(404); res.end('Not found');
  };
}

// ---------------------------------------------------------------------------
// Agent poll endpoints (unchanged from WS version)
// ---------------------------------------------------------------------------

function handlePollGet(req, res, url) {
  const token = url.searchParams.get('token');
  if (token !== state.agentToken) {
    res.writeHead(401, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: 'Unauthorized' }));
    return;
  }
  const timeout = parseBoundedPositiveInteger(
    url.searchParams.get('timeout'),
    DEFAULT_POLL_TIMEOUT,
    MAX_POLL_TIMEOUT,
  );
  const leaseMs = parseBoundedPositiveInteger(
    url.searchParams.get('leaseMs'),
    DEFAULT_LEASE_MS,
    MAX_LEASE_MS,
  );
  if (timeout === null || leaseMs === null) {
    res.writeHead(400, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: 'Invalid poll bounds' }));
    return;
  }
  const available = findAvailablePendingEvent();
  if (available) {
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(leaseEvent(available, leaseMs)));
    return;
  }
  const poll = { resolve, leaseMs };
  const timer = setTimeout(() => {
    const idx = state.pendingPolls.indexOf(poll);
    if (idx !== -1) state.pendingPolls.splice(idx, 1);
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ type: 'timeout' }));
  }, timeout);
  function resolve(event) {
    clearTimeout(timer);
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(event));
  }
  state.pendingPolls.push(poll);
  scheduleLeaseFlush();
  req.on('close', () => {
    clearTimeout(timer);
    const idx = state.pendingPolls.indexOf(poll);
    if (idx !== -1) state.pendingPolls.splice(idx, 1);
  });
}

function parseBoundedPositiveInteger(raw, fallback, maximum) {
  if (raw === null) return fallback;
  if (!/^[1-9][0-9]*$/.test(raw)) return null;
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value > maximum) return null;
  return value;
}

function handlePollPost(req, res) {
  readBoundedJsonBody(req, res, (msg) => {
    if (!isJsonObject(msg)) {
      res.writeHead(400, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Invalid agent reply' }));
      return;
    }
    if (msg.token !== state.agentToken) {
      res.writeHead(401, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Unauthorized' }));
      return;
    }
    if (!['agent_done', 'complete', 'discard', 'discarded', 'done', 'error'].includes(msg.type)) {
      res.writeHead(400, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Invalid agent reply type' }));
      return;
    }
    const leasedIndex = findLeasedPendingEventIndex(msg.id, msg.leaseToken);
    const leasedEvent = leasedIndex === -1 ? null : state.pendingEvents[leasedIndex].event;
    if (leasedIndex !== -1
      && !isCompatibleAgentReply(state.pendingEvents[leasedIndex].event, msg)) {
      res.writeHead(409, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Reply type does not match the leased event' }));
      return;
    }
    let matched = leasedIndex !== -1;
    let matchedCarbonizeCompletion = false;
    if (!matched
      && ['complete', 'error'].includes(msg.type)
      && state.sessionStore
      && msg.id) {
      try {
        matchedCarbonizeCompletion = state.sessionStore
          .getSnapshot(msg.id, { includeCompleted: true })?.phase === 'carbonize_required';
        matched = matchedCarbonizeCompletion;
      } catch {
        matched = false;
      }
    }
    if (!matched) {
      res.writeHead(409, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'No matching leased event' }));
      return;
    }
    const eventType = msg.type === 'discard' || msg.type === 'discarded'
      ? 'discarded'
      : msg.type === 'complete'
        ? 'complete'
        : msg.type === 'error'
          ? 'agent_error'
          : 'agent_done';
    if (state.sessionStore && msg.id) {
      try {
        state.sessionStore.appendEvent({
          type: eventType,
          id: msg.id,
          file: msg.file,
          message: msg.message,
          carbonize: msg.data?.carbonize === true,
        });
      } catch (error) {
        res.writeHead(error.code === 'live_session_limit' ? 507 : 500, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'session_store_append_failed', message: error.message }));
        return;
      }
    }
    if (leasedIndex !== -1) {
      const updated = acknowledgePendingEvent(leasedIndex, msg.id, msg.leaseToken);
      if (!updated) {
        res.writeHead(409, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ error: 'Leased event changed before acknowledgement' }));
        return;
      }
      if (msg.type === 'error' && leasedEvent?.id && leasedEvent.type) {
        state.seenBrowserEvents.delete(`${leasedEvent.id}:${leasedEvent.type}`);
      }
    }
    flushPendingPolls();
    // Forward the reply to the browser via SSE. Terminal attempt outcomes are
    // retained only in bounded process memory so a reconnect cannot miss its
    // acknowledgement; project journals never drive this replay path.
    const browserReply = {
      type: msg.type || 'done',
      id: msg.id,
      message: msg.message,
      file: msg.file,
      data: matchedCarbonizeCompletion
        ? { ...(msg.data || {}), cleanup: true }
        : msg.data,
    };
    const replayId = ['complete', 'discard', 'discarded', 'error'].includes(browserReply.type)
      ? rememberTerminalOutcome(browserReply)
      : null;
    broadcast(browserReply, replayId);
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ ok: true }));
  });
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

let httpServer = null;

function cleanupSessionDirectory() {
  if (!state.sessionDir) return null;
  const sessionDir = state.sessionDir;
  try {
    fs.rmSync(sessionDir, {
      recursive: true,
      force: true,
      maxRetries: SESSION_CLEANUP_RETRIES,
      retryDelay: SESSION_CLEANUP_RETRY_DELAY_MS,
    });
    state.sessionDir = null;
    return null;
  } catch (error) {
    return {
      error: 'live_session_cleanup_failed',
      code: typeof error?.code === 'string' ? error.code : 'CLEANUP_FAILED',
      path: sessionDir,
      pid: process.pid,
      message: error?.message || String(error),
    };
  }
}

function cleanupRuntimeState() {
  if (state.leaseTimer) clearTimeout(state.leaseTimer);
  state.leaseTimer = null;
  const cleanupError = cleanupSessionDirectory();
  state.annotationFiles.clear();
  state.annotationBytes = 0;
  state.annotationUploads = 0;
  state.annotationInflightBytes = 0;
  state.activeUploads.clear();
  for (const res of state.sseClients) { try { res.end(); } catch {} }
  state.sseClients.clear();
  for (const poll of state.pendingPolls.splice(0)) poll.resolve({ type: 'exit' });
  state.pendingPolls.length = 0;
  state.releaseServerRecordLock?.();
  state.releaseServerRecordLock = null;
  return cleanupError;
}

let shuttingDown = false;

function quiesceRuntimeConnections() {
  if (state.exitTimer) clearTimeout(state.exitTimer);
  state.exitTimer = null;
  if (state.leaseTimer) clearTimeout(state.leaseTimer);
  state.leaseTimer = null;
  for (const res of state.sseClients) { try { res.end(); } catch {} }
  for (const poll of state.pendingPolls.splice(0)) poll.resolve({ type: 'exit' });
  for (const req of state.activeUploads) { try { req.destroy(); } catch {} }
}

async function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  let releaseLock = state.releaseServerRecordLock;
  let shutdownLockAcquired = Boolean(releaseLock);
  state.releaseServerRecordLock = null;
  try {
    releaseLock ||= await acquireServerRecordLock(
      Math.max(1_000, LIVE_SERVER_STOP_TIMEOUT_MS - SHUTDOWN_LOCK_MARGIN_MS),
    );
    shutdownLockAcquired = true;
  } catch {
    // Cleanup remains authoritative; retain the record if ownership cannot be
    // reacquired so the stop command can report an incomplete shutdown.
  }
  // Close long-lived connections and retain upload/session state until the
  // HTTP server has stopped listening.
  quiesceRuntimeConnections();
  const finishShutdown = () => {
    const sessionCleanupError = cleanupRuntimeState();
    if (sessionCleanupError) console.error(JSON.stringify(sessionCleanupError));
    if (!sessionCleanupError && shutdownLockAcquired) {
      const record = readLiveServerInfo(process.cwd());
      if (record?.info?.pid === process.pid && record.info.token === state.token) {
        removeServerRecordIfUnchanged(record);
      }
    }
    releaseLock?.();
    process.exit(sessionCleanupError ? 1 : 0);
  };
  if (httpServer?.listening) {
    httpServer.close(finishShutdown);
    // A partial upload has no useful completion once shutdown is owned. Abort
    // remaining HTTP connections so cleanup cannot wait on an abandoned body.
    httpServer.closeIdleConnections?.();
    httpServer.closeAllConnections?.();
  } else {
    finishShutdown();
  }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

const args = process.argv.slice(2);

if (args.includes('--help') || args.includes('-h')) {
  console.log(`Usage: node live-server.mjs [options]

Start the live variant mode server (zero dependencies).

Commands:
  (default)     Start the server (foreground)
  stop          Stop the server and remove the injected live.js script tag
  stop --keep-inject   Stop the server only (leave the script tag in the HTML entry)

Options:
  --background  Start detached, print non-secret process/port JSON, then exit
  --port=PORT   Use a specific port (default: auto-detect starting at 8400)
  --keep-inject Only with stop: skip live-inject.mjs --remove
  --help        Show this help

Endpoints:
  /live.js             Browser script (element picker + variant cycling)
  /detect.js           Detection overlay (backwards compatible)
  /modern-screenshot.js Vendored modern-screenshot UMD build (lazy-loaded by live.js)
  /annotation          POST raw image/png to stage a variant screenshot
  /events              SSE stream (server→browser) + POST (browser→server)
  /poll                Long-poll for agent CLI
  /source              Raw source file reader (no-HMR fallback)
  /status              Durable session status (token-protected)
  /health              Health check`);
  process.exit(0);
}

try {
  ensureCanonicalLiveStateRoot(process.cwd());
} catch (error) {
  console.error(JSON.stringify({
    error: error.code || 'live_state_root_invalid',
    message: error.message,
  }));
  process.exit(1);
}

if (args.includes('stop')) {
  const keepInject = args.includes('--keep-inject');
  let releaseStopLock = null;
  let stopExitCode = 0;
  try {
    releaseStopLock = await acquireServerRecordLock();
    const record = readLiveServerInfo(process.cwd());
    if (record?.info && await probeServerInfo(record.info)) {
      const { info } = record;
      let response;
      try {
        response = await fetch(`http://127.0.0.1:${info.port}/stop?token=${encodeURIComponent(info.token)}`, {
          signal: AbortSignal.timeout(LIVE_SERVER_STOP_TIMEOUT_MS),
        });
      } catch (error) {
        console.error(JSON.stringify({
          error: 'live_server_stop_failed',
          pid: info.pid,
          port: info.port,
          path: record.path,
          reason: error?.name === 'TimeoutError' ? 'stop_request_timeout' : 'stop_request_error',
          timeoutMs: LIVE_SERVER_STOP_TIMEOUT_MS,
          message: error.message,
        }));
        stopExitCode = 1;
      }
      if (response?.ok) {
        // The server needs this same lock to finish its shutdown. Release the
        // stop-command lock before waiting, then reacquire it for final
        // record cleanup after the exact process has exited.
        releaseStopLock?.();
        releaseStopLock = null;
        if (await waitForProcessExit(info.pid, { timeoutMs: LIVE_SERVER_STOP_TIMEOUT_MS })) {
          try {
            releaseStopLock = await acquireServerRecordLock();
          } catch (error) {
            console.error(JSON.stringify({
              error: 'live_server_stop_failed',
              pid: info.pid,
              port: info.port,
              path: record.path,
              timeoutMs: LIVE_SERVER_STOP_TIMEOUT_MS,
              reason: 'record_lock_error',
              message: error.message,
            }));
            stopExitCode = 1;
          }
          if (releaseStopLock) {
            const finalRecord = readLiveServerInfo(process.cwd());
            if (!finalRecord) {
              console.log(`Stopped live server on port ${info.port}.`);
            } else if (finalRecord.info?.pid === info.pid
              && finalRecord.info?.port === info.port
              && finalRecord.info?.token === info.token) {
              console.error(JSON.stringify({
                error: 'server_cleanup_incomplete',
                pid: info.pid,
                port: info.port,
                path: record.path,
                timeoutMs: LIVE_SERVER_STOP_TIMEOUT_MS,
                message: 'The live server exited but retained its state record; inspect the private state path before retrying cleanup.',
              }));
              stopExitCode = 1;
            } else {
              console.error(JSON.stringify({
                error: 'live_server_record_changed',
                pid: info.pid,
                port: info.port,
                path: record.path,
                timeoutMs: LIVE_SERVER_STOP_TIMEOUT_MS,
                message: 'The live server exited but its state record changed; refusing to remove the current record.',
              }));
              stopExitCode = 1;
            }
          }
        } else {
          console.error(JSON.stringify({
            error: 'live_server_stop_timeout',
            pid: info.pid,
            port: info.port,
            path: record.path,
            timeoutMs: LIVE_SERVER_STOP_TIMEOUT_MS,
            message: 'The owned live server did not exit within the bounded shutdown window.',
          }));
          stopExitCode = 1;
        }
      } else if (response && !response.ok) {
        console.error(JSON.stringify({
          error: 'live_server_stop_failed',
          pid: info.pid,
          port: info.port,
          path: record.path,
          reason: 'stop_request_failed',
          httpStatus: response.status,
        }));
        stopExitCode = 1;
      }
    } else if (record?.info && isProcessAlive(record.info.pid)) {
      console.error(JSON.stringify({
        error: 'live_server_probe_failed',
        pid: record.info.pid,
        port: record.info.port,
        path: record.path,
        message: 'The recorded live-server PID is still alive but its authenticated probe failed; state was preserved.',
      }));
      stopExitCode = 1;
    } else {
      removeServerRecordIfUnchanged(record);
      console.log('No running live server found.');
    }
  } catch (error) {
    console.error(JSON.stringify({
      error: 'live_server_stop_failed',
      reason: 'stop_command_error',
      message: error.message,
    }));
    stopExitCode = 1;
  } finally {
    releaseStopLock?.();
  }
  if (!keepInject) {
    const injectPath = path.join(__dirname, 'live-inject.mjs');
    try {
      const out = execFileSync(process.execPath, [injectPath, '--remove'], {
        encoding: 'utf-8',
        cwd: process.cwd(),
      });
      const line = out.trim().split('\n').filter(Boolean).pop();
      if (line) {
        try {
          const j = JSON.parse(line);
          if (j.removed === true) {
            console.log(`Removed live script tag from ${j.file}.`);
          }
        } catch {
          /* ignore non-JSON lines */
        }
      }
    } catch (err) {
      const detail = err.stderr?.toString?.().trim?.()
        || err.stdout?.toString?.().trim?.()
        || err.message
        || String(err);
      console.warn(`Note: could not remove live script tag (${detail.split('\n')[0]})`);
    }
  }
  process.exit(stopExitCode);
}

// --background: spawn a detached child server, wait for it to be ready,
// print the connection JSON, then exit.  This keeps the startup command
// simple (no shell backgrounding or chained commands).
if (args.includes('--background')) {
  const childArgs = args.filter(a => a !== '--background');
  const startedAt = Date.now();
  const child = spawn(process.execPath, [fileURLToPath(import.meta.url), ...childArgs], {
    detached: true,
    stdio: 'ignore',
    cwd: process.cwd(),
  });
  let childExit = null;
  child.once('exit', (code, signal) => {
    childExit = { code, signal };
  });
  child.once('error', (error) => {
    childExit = { code: null, signal: null, error: { code: error.code, message: error.message } };
  });
  child.unref();

  // Poll for the PID file (the child writes it once the HTTP server is listening).
  const deadline = startedAt + LIVE_SERVER_STARTUP_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (childExit) {
      const outcome = classifyStartupOutcome({
        exit: childExit,
        observation: observeStartup({
          startedAt,
          observedAt: Date.now(),
        }),
      });
      console.error(JSON.stringify({
        ...outcome,
        message: 'Live server refused to start; check bind or startup errors.',
      }));
      process.exit(1);
    }
    try {
      const { info } = readLiveServerInfo(process.cwd()) || {};
      if (info?.pid === child.pid && await probeServerInfo(info)) {
        // Browser bearer state stays in server.json and the agent credential
        // stays outside the project tree; stdout receives only process identity.
        console.log(JSON.stringify({ pid: info.pid, port: info.port }));
        process.exit(0);
      }
    } catch { /* not ready yet */ }
    await new Promise(r => setTimeout(r, 200));
  }
  const outcome = classifyStartupOutcome({
    observation: observeStartup({
      startedAt,
      observedAt: Date.now(),
    }),
  });
  console.error(JSON.stringify({
    ...outcome,
    message: 'Timed out waiting for live server to start.',
  }));
  try { process.kill(child.pid, 'SIGTERM'); } catch {}
  process.exit(1);
}

// Serialize record inspection, stale cleanup, bind, and publication so a
// concurrent start/stop cannot delete or claim another process's record.
try {
  state.releaseServerRecordLock = await acquireServerRecordLock();
} catch (error) {
  console.error(JSON.stringify({ error: error.code, message: error.message }));
  process.exit(1);
}
process.once('exit', () => state.releaseServerRecordLock?.());

// Check for existing session
const existingRecord = readLiveServerInfo(process.cwd());
if (existingRecord?.info) {
  const existing = existingRecord.info;
  if (await probeServerInfo(existing)) {
    console.error(`Live server already running on port ${existing.port} (pid ${existing.pid}).`);
    console.error('Stop it first with: node ' + path.basename(fileURLToPath(import.meta.url)) + ' stop');
    state.releaseServerRecordLock?.();
    state.releaseServerRecordLock = null;
    process.exit(1);
  }
  removeServerRecordIfUnchanged(existingRecord);
}

const portArg = args.find(a => a.startsWith('--port='));
if (portArg) {
  const rawPort = portArg.slice('--port='.length);
  if (!/^[1-9][0-9]*$/.test(rawPort)
      || !Number.isSafeInteger(Number(rawPort))
      || Number(rawPort) > 65535) {
    console.error(JSON.stringify({ error: 'invalid_port', value: rawPort }));
    process.exit(1);
  }
}

let browserScripts;
try {
  browserScripts = loadBrowserScripts();
} catch (error) {
  console.error(JSON.stringify({
    error: 'ui_evidence_runtime_unavailable',
    message: error.message,
  }));
  process.exit(1);
}

state.token = randomUUID();
state.agentToken = randomUUID();
state.sessionStore = createLiveSessionStore({ cwd: process.cwd() });
// Durable journals remain available to explicit inspection through
// live-resume.mjs, but they are project-controlled inputs and never establish
// authority to enqueue work in a new server process.
state.port = portArg ? Number(portArg.slice('--port='.length)) : await findOpenPort();
// Keep annotation output in one fresh, private OS-temporary run directory.
// Shutdown removes only this exact directory.
state.sessionDir = fs.mkdtempSync(path.join(os.tmpdir(), 'impeccable-live-'));
fs.chmodSync(state.sessionDir, 0o700);
state.agentStatePath = writeLiveAgentServerInfo(state.sessionDir, {
  pid: process.pid,
  port: state.port,
  agentToken: state.agentToken,
});

const { detectScript, sessionPath, livePath } = browserScripts;
httpServer = http.createServer(createRequestHandler({ detectScript, sessionPath, livePath }));

httpServer.once('error', (error) => {
  const cleanupError = cleanupRuntimeState();
  if (cleanupError) console.error(JSON.stringify(cleanupError));
  console.error(JSON.stringify({
    error: 'live_server_bind_failed',
    code: typeof error?.code === 'string' ? error.code : 'BIND_FAILED',
    port: state.port,
    message: `Unable to bind live server to 127.0.0.1:${state.port}`,
  }));
  process.exit(1);
});

httpServer.listen(state.port, '127.0.0.1', () => {
  writeLiveServerInfo(process.cwd(), {
    pid: process.pid,
    port: state.port,
    token: state.token,
    agentStatePath: state.agentStatePath,
  });
  state.releaseServerRecordLock?.();
  state.releaseServerRecordLock = null;
  const url = `http://127.0.0.1:${state.port}`;
  console.log(`\nImpeccable live server running on ${url}`);
  console.log('Browser credential stored in .impeccable/live/server.json; agent credential stored outside the project tree.');
  console.log(`Stop:   node ${path.basename(fileURLToPath(import.meta.url))} stop`);
});

process.on('SIGINT', () => void shutdown());
process.on('SIGTERM', () => void shutdown());
