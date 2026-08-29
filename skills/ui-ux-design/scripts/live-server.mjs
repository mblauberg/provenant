#!/usr/bin/env node
// Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md.
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
import path from 'node:path';
import net from 'node:net';
import { fileURLToPath } from 'node:url';
import { parseDesignMd } from './design-parser.mjs';
import { loadContext, resolveContextDir } from './load-context.mjs';
import { resolveFiles } from './live-inject.mjs';
import { createLiveSessionStore } from './live-session-store.mjs';
import {
  getDesignSidecarPath,
  getLiveAnnotationsDir,
  readLiveServerInfo,
  removeLiveServerInfo,
  resolveLiveConfigPath,
  resolveDesignSidecarPath,
  writeLiveServerInfo,
} from './impeccable-paths.mjs';
import {
  classifyStartupOutcome,
  LIVE_SERVER_STARTUP_TIMEOUT_MS,
  observeStartup,
} from './live-server-startup.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
// PRODUCT.md / DESIGN.md live wherever load-context.mjs resolves. The generated
// DESIGN sidecar is project-local at .impeccable/design.json, with legacy
// DESIGN.json fallback for existing projects.
const CONTEXT_DIR = resolveContextDir(process.cwd());
const DEFAULT_POLL_TIMEOUT = 600_000;   // 10 min — agent re-polls on timeout anyway
const SSE_HEARTBEAT_INTERVAL = 30_000;  // keepalive ping every 30s
const SAFE_SOURCE_EXTENSIONS = new Set([
  '.astro', '.htm', '.html', '.jsx', '.svelte', '.tsx', '.vue',
]);

// ---------------------------------------------------------------------------
// Port detection
// ---------------------------------------------------------------------------

async function findOpenPort(start = 8400) {
  return new Promise((resolve) => {
    const srv = net.createServer();
    srv.listen(start, '127.0.0.1', () => {
      const port = srv.address().port;
      srv.close(() => resolve(port));
    });
    srv.on('error', () => resolve(findOpenPort(start + 1)));
  });
}

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

const state = {
  token: null,
  port: null,
  sseClients: new Set(),   // SSE response objects (server→browser push)
  pendingEvents: [],        // browser events waiting for agent ack ({ event, leaseUntil })
  pendingPolls: [],         // agent poll callbacks waiting for browser events
  exitTimer: null,
  sessionDir: null,         // per-session tmp dir for annotation screenshots
  sessionStore: null,
  leaseTimer: null,
};

// Cap per-annotation upload size. A full 1920×1080 PNG is typically <1 MB;
// cap at 10 MB to guard against runaway writes from a misbehaving client.
const MAX_ANNOTATION_BYTES = 10 * 1024 * 1024;
const MAX_JSON_BODY_BYTES = 256 * 1024;

function enqueueEvent(event) {
  if (!event || (event.id && state.pendingEvents.some((entry) => entry.event?.id === event.id && entry.event?.type === event.type))) return;
  state.pendingEvents.push({ event, leaseUntil: 0 });
  flushPendingPolls();
}

function restorePendingEventsFromStore() {
  if (!state.sessionStore) return;
  for (const snapshot of state.sessionStore.listActiveSessions()) {
    if (snapshot.pendingEvent) enqueueEvent(snapshot.pendingEvent);
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
  return entry.event;
}

function acknowledgePendingEvent(id) {
  if (!id) return false;
  const idx = state.pendingEvents.findIndex((entry) => entry.event?.id === id);
  if (idx === -1) return false;
  state.pendingEvents.splice(idx, 1);
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

/** Push a message to all connected SSE clients. */
function broadcast(msg) {
  const data = 'data: ' + JSON.stringify(msg) + '\n\n';
  for (const res of state.sseClients) {
    try { res.write(data); } catch { /* client gone */ }
  }
}

// ---------------------------------------------------------------------------
// Load scripts
// ---------------------------------------------------------------------------

function loadBrowserScripts() {
  // Detection script: prefer the skill-bundled detector, then fall back to
  // source/npm package locations for local development and older installs.
  // This one IS cached — detect.js rarely changes during a session.
  const detectPaths = [
    path.join(__dirname, 'detector', 'detect-antipatterns-browser.js'),
    path.join(__dirname, '..', '..', 'cli', 'engine', 'detect-antipatterns-browser.js'),
    path.join(__dirname, '..', '..', '..', '..', 'cli', 'engine', 'detect-antipatterns-browser.js'),
    path.join(process.cwd(), 'node_modules', 'impeccable', 'cli', 'engine', 'detect-antipatterns-browser.js'),
  ];
  let detectScript = '';
  for (const p of detectPaths) {
    try { detectScript = fs.readFileSync(p, 'utf-8'); break; } catch { /* try next */ }
  }

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

function statOrNull(filePath) {
  try { return fs.statSync(filePath); } catch { return null; }
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
const VARIANT_ID_PATTERN = /^[0-9]{1,3}$/;

function isValidId(v) { return typeof v === 'string' && ID_PATTERN.test(v); }
function isValidVariantId(v) { return typeof v === 'string' && VARIANT_ID_PATTERN.test(v); }

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

  const configPath = resolveLiveConfigPath({ cwd: rootDir, scriptsDir: __dirname });
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
  return sourceReal;
}

function createRequestHandler({ detectScript, sessionPath, livePath }) {
  return (req, res) => {
    const url = new URL(req.url, `http://localhost:${state.port}`);
    const p = url.pathname;
    if (req.method === 'OPTIONS') {
      applyCors(req, res);
      res.writeHead(204);
      res.end();
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
      const chunks = [];
      let total = 0;
      let aborted = false;
      req.on('data', (c) => {
        if (aborted) return;
        total += c.length;
        if (total > MAX_ANNOTATION_BYTES) {
          aborted = true;
          res.writeHead(413, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Payload too large' }));
          req.destroy();
          return;
        }
        chunks.push(c);
      });
      req.on('end', () => {
        if (aborted) return;
        const absPath = path.join(state.sessionDir, eventId + '.png');
        try {
          fs.writeFileSync(absPath, Buffer.concat(chunks), { flag: 'wx', mode: 0o600 });
          fs.chmodSync(absPath, 0o600);
        } catch (err) {
          res.writeHead(err.code === 'EEXIST' ? 409 : 500, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Write failed: ' + err.message }));
          return;
        }
        res.writeHead(200, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ ok: true, path: absPath }));
      });
      req.on('error', () => {
        if (!aborted) {
          res.writeHead(500, { 'Content-Type': 'application/json' });
          res.end(JSON.stringify({ error: 'Upload failed' }));
        }
      });
      return;
    }

    // --- Health ---
    if (p === '/status') {
      if (!authenticateQuery(req, res, url)) return;
      const sessions = state.sessionStore ? state.sessionStore.listActiveSessions() : [];
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({
        status: 'ok',
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
        status: 'ok', port: state.port, mode: 'variant',
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
      const jsonPath = resolveDesignSidecarPath(process.cwd(), CONTEXT_DIR) || getDesignSidecarPath(process.cwd());
      const mdStat = statOrNull(mdPath);
      const jsonStat = statOrNull(jsonPath);

      if (p === '/design-system/raw') {
        if (!mdStat) { res.writeHead(404); res.end('Not found'); return; }
        res.writeHead(200, { 'Content-Type': 'text/markdown; charset=utf-8' });
        res.end(fs.readFileSync(mdPath, 'utf-8'));
        return;
      }

      if (!mdStat && !jsonStat) {
        res.writeHead(404, { 'Content-Type': 'application/json' });
        res.end(JSON.stringify({ present: false }));
        return;
      }

      const response = {
        present: true,
        hasMd: !!mdStat,
        hasSidecar: !!jsonStat,
        mdNewerThanJson: !!(mdStat && jsonStat && mdStat.mtimeMs > jsonStat.mtimeMs + 1000),
      };

      if (mdStat) {
        try {
          response.parsed = parseDesignMd(fs.readFileSync(mdPath, 'utf-8'));
        } catch (err) {
          response.parseError = err.message;
        }
      }

      if (jsonStat) {
        try {
          response.sidecar = JSON.parse(fs.readFileSync(jsonPath, 'utf-8'));
        } catch (err) {
          response.sidecarError = 'Failed to parse .impeccable/design.json: ' + err.message;
        }
      }

      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(response));
      return;
    }

    // --- Source file (no-HMR fallback) ---
    if (p === '/source') {
      if (!authenticateQuery(req, res, url)) return;
      const filePath = url.searchParams.get('path');
      let absPath;
      try {
        absPath = resolveConfiguredProjectSource(process.cwd(), filePath);
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
      const content = fs.readFileSync(absPath, 'utf-8');
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      res.end(content);
      return;
    }

    // --- SSE: server→browser push (replaces WebSocket) ---
    if (p === '/events' && req.method === 'GET') {
      if (!authenticateQuery(req, res, url)) return;
      res.writeHead(200, {
        'Content-Type': 'text/event-stream',
        'Cache-Control': 'no-cache',
        'Connection': 'keep-alive',
      });
      res.write('data: ' + JSON.stringify({
        type: 'connected',
        hasProjectContext: hasProjectContext(),
      }) + '\n\n');

      state.sseClients.add(res);
      clearTimeout(state.exitTimer);

      // Keepalive: SSE comment every 30s prevents silent connection drops.
      const heartbeat = setInterval(() => {
        try { res.write(': keepalive\n\n'); } catch { clearInterval(heartbeat); }
      }, SSE_HEARTBEAT_INTERVAL);

      req.on('close', () => {
        clearInterval(heartbeat);
        state.sseClients.delete(res);
        if (state.sseClients.size === 0) {
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
        if (state.sessionStore && msg.id) {
          try {
            state.sessionStore.appendEvent(msg);
          } catch (err) {
            res.writeHead(500, { 'Content-Type': 'application/json' });
            res.end(JSON.stringify({ error: 'session_store_append_failed', message: err.message }));
            return;
          }
        }
        if (msg.type !== 'checkpoint') enqueueEvent(msg);
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
      shutdown();
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
  if (token !== state.token) {
    res.writeHead(401, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: 'Unauthorized' }));
    return;
  }
  const timeout = parseInt(url.searchParams.get('timeout') || DEFAULT_POLL_TIMEOUT, 10);
  const leaseMs = parseInt(url.searchParams.get('leaseMs') || '30000', 10);
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

function handlePollPost(req, res) {
  readBoundedJsonBody(req, res, (msg) => {
    if (msg.token !== state.token) {
      res.writeHead(401, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ error: 'Unauthorized' }));
      return;
    }
    acknowledgePendingEvent(msg.id);
    if (state.sessionStore && msg.id) {
      try {
        const eventType = msg.type === 'discard' || msg.type === 'discarded'
          ? 'discarded'
          : msg.type === 'complete'
            ? 'complete'
            : msg.type === 'error'
              ? 'agent_error'
              : 'agent_done';
        state.sessionStore.appendEvent({
          type: eventType,
          id: msg.id,
          file: msg.file,
          message: msg.message,
          carbonize: msg.data?.carbonize === true,
        });
      } catch { /* keep reply path best-effort; browser still needs SSE */ }
    }
    flushPendingPolls();
    // Forward the reply to the browser via SSE
    broadcast({ type: msg.type || 'done', id: msg.id, message: msg.message, file: msg.file, data: msg.data });
    res.writeHead(200, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ ok: true }));
  });
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

let httpServer = null;

function shutdown() {
  removeLiveServerInfo(process.cwd());
  if (state.leaseTimer) clearTimeout(state.leaseTimer);
  state.leaseTimer = null;
  if (state.sessionDir) {
    try { fs.rmSync(state.sessionDir, { recursive: true, force: true }); } catch {}
  }
  for (const res of state.sseClients) { try { res.end(); } catch {} }
  state.sseClients.clear();
  for (const poll of state.pendingPolls) poll.resolve({ type: 'exit' });
  state.pendingPolls.length = 0;
  if (httpServer) httpServer.close();
  process.exit(0);
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
  /status              Durable recovery status (token-protected)
  /health              Health check`);
  process.exit(0);
}

if (args.includes('stop')) {
  const keepInject = args.includes('--keep-inject');
  try {
    const { info } = readLiveServerInfo(process.cwd()) || {};
    const res = await fetch(`http://localhost:${info.port}/stop?token=${info.token}`);
    if (res.ok) console.log(`Stopped live server on port ${info.port}.`);
  } catch {
    console.log('No running live server found.');
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
  process.exit(0);
}

// --background: spawn a detached child server, wait for it to be ready,
// print the connection JSON, then exit.  This keeps the startup command
// simple (no shell backgrounding or chained commands).
if (args.includes('--background')) {
  const childArgs = args.filter(a => a !== '--background');
  const startedAt = Date.now();
  const previousInfo = readLiveServerInfo(process.cwd())?.info || null;
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
      const replacedPreviousRecord = !previousInfo
        || info.pid !== previousInfo.pid
        || info.token !== previousInfo.token;
      if (info.pid !== process.pid && replacedPreviousRecord) {
        // Bearer state stays in private server.json; terminal/transcript output
        // receives only the non-secret process identity needed for startup.
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
  process.exit(1);
}

// Check for existing session
const existingRecord = readLiveServerInfo(process.cwd());
if (existingRecord?.info) {
  const existing = existingRecord.info;
  try {
    process.kill(existing.pid, 0);
    console.error(`Live server already running on port ${existing.port} (pid ${existing.pid}).`);
    console.error('Stop it first with: node ' + path.basename(fileURLToPath(import.meta.url)) + ' stop');
    process.exit(1);
  } catch {
    try { fs.unlinkSync(existingRecord.path); } catch {}
  }
}

state.token = randomUUID();
state.sessionStore = createLiveSessionStore({ cwd: process.cwd() });
restorePendingEventsFromStore();
const portArg = args.find(a => a.startsWith('--port='));
state.port = portArg ? parseInt(portArg.split('=')[1], 10) : await findOpenPort();
// Annotation screenshots live in the project root so the agent's Read tool
// doesn't trip a per-file permission prompt. Sessioned by token so concurrent
// projects (or quick restarts) don't collide.
const annotRoot = getLiveAnnotationsDir(process.cwd());
fs.mkdirSync(annotRoot, { recursive: true });
state.sessionDir = fs.mkdtempSync(path.join(annotRoot, 'session-'));

const { detectScript, sessionPath, livePath } = loadBrowserScripts();
httpServer = http.createServer(createRequestHandler({ detectScript, sessionPath, livePath }));

httpServer.listen(state.port, '127.0.0.1', () => {
  writeLiveServerInfo(process.cwd(), { pid: process.pid, port: state.port, token: state.token });
  const url = `http://localhost:${state.port}`;
  console.log(`\nImpeccable live server running on ${url}`);
  console.log('Bearer state stored privately in .impeccable/live/server.json.');
  console.log(`Stop:   node ${path.basename(fileURLToPath(import.meta.url))} stop`);
});

process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
