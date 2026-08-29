// Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md.
import fs from 'node:fs';
import path from 'node:path';
import {
  ensureCanonicalLiveStateRoot,
  getLegacyLiveSessionsDir,
  getLiveSessionsDir,
} from './impeccable-paths.mjs';

const COMPLETED_PHASES = new Set(['completed', 'discarded']);

export function createLiveSessionStore({ cwd = process.cwd(), sessionId } = {}) {
  ensureCanonicalLiveStateRoot(cwd);
  const rootDir = ensureCanonicalSessionRoot(cwd);
  const rootIdentity = directoryIdentity(rootDir);
  const legacyRootDir = getLegacyLiveSessionsDir(cwd);
  const legacyIdentity = optionalCanonicalDirectoryIdentity(legacyRootDir);
  const snapshotCache = new Map();
  const assertRoot = () => assertDirectoryIdentity(rootDir, rootIdentity);
  const assertLegacy = legacyIdentity
    ? () => assertDirectoryIdentity(legacyRootDir, legacyIdentity)
    : null;

  function loadCachedOrRebuild(id) {
    assertRoot();
    const cached = snapshotCache.get(id);
    if (cached) return cached;
    const journal = getReadableJournalPath(id);
    const rebuilt = rebuildSnapshotFromJournal(journal.path, id, journal.assertParent);
    snapshotCache.set(id, rebuilt);
    return rebuilt;
  }

  function getReadableJournalPath(id) {
    assertRoot();
    const primary = getJournalPath(rootDir, id);
    if (fs.existsSync(primary)) return { path: primary, assertParent: assertRoot };
    const legacy = getJournalPath(legacyRootDir, id);
    if (assertLegacy) {
      assertLegacy();
      if (fs.existsSync(legacy)) return { path: legacy, assertParent: assertLegacy };
    }
    return { path: primary, assertParent: assertRoot };
  }

  return {
    rootDir,
    legacyRootDir,
    appendEvent(event) {
      assertRoot();
      const normalized = normalizeEvent(event, sessionId);
      const journalPath = getJournalPath(rootDir, normalized.id);
      const snapshotPath = getSnapshotPath(rootDir, normalized.id);
      const legacyJournalPath = getJournalPath(legacyRootDir, normalized.id);
      if (assertLegacy) assertLegacy();
      if (assertLegacy && !fs.existsSync(journalPath) && fs.existsSync(legacyJournalPath)) {
        writeStateFile(
          journalPath,
          readStateFile(legacyJournalPath, assertLegacy),
          fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_EXCL,
          assertRoot,
        );
      }
      const prior = loadCachedOrRebuild(normalized.id);
      const seq = prior.nextSeq;
      const entry = {
        seq,
        id: normalized.id,
        type: normalized.type,
        ts: new Date().toISOString(),
        event: normalized,
      };
      writeStateFile(
        journalPath,
        JSON.stringify(entry) + '\n',
        fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_APPEND,
        assertRoot,
      );
      const next = applyEvent(prior.snapshot, entry, prior.diagnostics);
      snapshotCache.set(normalized.id, { snapshot: next, diagnostics: next.diagnostics || [], nextSeq: seq + 1 });
      writeSnapshot(snapshotPath, next, assertRoot);
      return next;
    },
    getSnapshot(id = sessionId, opts = {}) {
      if (!id) throw new Error('session id required');
      assertRoot();
      const journal = getReadableJournalPath(id);
      const snapshotPath = getSnapshotPath(rootDir, id);
      const rebuilt = rebuildSnapshotFromJournal(journal.path, id, journal.assertParent);
      snapshotCache.set(id, rebuilt);
      writeSnapshot(snapshotPath, rebuilt.snapshot, assertRoot);
      if (!opts.includeCompleted && COMPLETED_PHASES.has(rebuilt.snapshot.phase)) return null;
      return rebuilt.snapshot;
    },
    listActiveSessions() {
      assertRoot();
      const ids = new Set();
      const directories = assertLegacy ? [legacyRootDir, rootDir] : [rootDir];
      for (const dir of directories) {
        if (dir === legacyRootDir) assertLegacy();
        if (!fs.existsSync(dir)) continue;
        for (const name of fs.readdirSync(dir)) {
          if (name.endsWith('.jsonl')) ids.add(name.slice(0, -'.jsonl'.length));
        }
      }
      return [...ids]
        .sort()
        .map((id) => this.getSnapshot(id))
        .filter(Boolean);
    },
  };
}

function ensureCanonicalSessionRoot(cwd) {
  const rootDir = getLiveSessionsDir(cwd);
  try {
    if (!fs.existsSync(rootDir)) fs.mkdirSync(rootDir);
    const metadata = fs.lstatSync(rootDir);
    if (metadata.isSymbolicLink() || !metadata.isDirectory()) {
      throw new Error('session state path is not a real directory');
    }
    if (fs.realpathSync.native(rootDir) !== path.resolve(rootDir)) {
      throw new Error('session state path is non-canonical');
    }
  } catch (cause) {
    const error = new Error('live_state_root_invalid: session state root must be canonical', {
      cause,
    });
    error.code = 'live_state_root_invalid';
    throw error;
  }
  return rootDir;
}

function directoryIdentity(directory) {
  const metadata = fs.lstatSync(directory);
  return { dev: metadata.dev, ino: metadata.ino };
}

function optionalCanonicalDirectoryIdentity(directory) {
  try {
    const metadata = fs.lstatSync(directory);
    if (metadata.isSymbolicLink()
      || !metadata.isDirectory()
      || fs.realpathSync.native(directory) !== path.resolve(directory)) {
      return null;
    }
    return { dev: metadata.dev, ino: metadata.ino };
  } catch {
    return null;
  }
}

function assertDirectoryIdentity(directory, expected) {
  try {
    const metadata = fs.lstatSync(directory);
    if (metadata.isSymbolicLink()
      || !metadata.isDirectory()
      || fs.realpathSync.native(directory) !== path.resolve(directory)
      || metadata.dev !== expected.dev
      || metadata.ino !== expected.ino) {
      throw new Error('session state root identity changed');
    }
  } catch (cause) {
    const error = new Error('live_state_root_invalid: session state root identity changed', {
      cause,
    });
    error.code = 'live_state_root_invalid';
    throw error;
  }
}

function readStateFile(filePath, assertParent) {
  const descriptor = openStateFile(filePath, fs.constants.O_RDONLY, assertParent);
  try {
    return fs.readFileSync(descriptor, 'utf8');
  } finally {
    fs.closeSync(descriptor);
  }
}

function writeStateFile(filePath, content, flags, assertParent) {
  const truncate = (flags & fs.constants.O_TRUNC) !== 0;
  const descriptor = openStateFile(filePath, flags & ~fs.constants.O_TRUNC, assertParent);
  try {
    if (truncate) fs.ftruncateSync(descriptor, 0);
    fs.writeFileSync(descriptor, content, 'utf8');
  } finally {
    fs.closeSync(descriptor);
  }
}

function openStateFile(filePath, flags, assertParent) {
  if (!Number.isInteger(fs.constants.O_NOFOLLOW)) {
    throw new Error('State-file writes require O_NOFOLLOW support');
  }
  assertParent?.();
  const descriptor = fs.openSync(filePath, flags | fs.constants.O_NOFOLLOW, 0o600);
  try {
    assertParent?.();
    const metadata = fs.fstatSync(descriptor);
    if (metadata.isFile() && metadata.nlink === 1) return descriptor;
    const error = new Error('Session state file must be a single-link regular file');
    error.code = 'live_state_file_invalid';
    throw error;
  } catch (error) {
    fs.closeSync(descriptor);
    throw error;
  }
}

function normalizeEvent(event, fallbackId) {
  if (!event || typeof event !== 'object') throw new Error('event object required');
  const id = event.id || fallbackId;
  if (!id || typeof id !== 'string') throw new Error('event id required');
  if (!event.type || typeof event.type !== 'string') throw new Error('event type required');
  const {
    token: _bearerToken,
    screenshotPath: _transientScreenshotPath,
    ...safeEvent
  } = event;
  return { ...safeEvent, id };
}

function getJournalPath(rootDir, id) {
  return path.join(rootDir, safeSessionId(id) + '.jsonl');
}

function getSnapshotPath(rootDir, id) {
  return path.join(rootDir, safeSessionId(id) + '.snapshot.json');
}

function safeSessionId(id) {
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(id)) throw new Error('invalid session id: ' + id);
  return id;
}

function baseSnapshot(id) {
  return {
    id,
    phase: 'new',
    pageUrl: null,
    sourceFile: null,
    expectedVariants: 0,
    arrivedVariants: 0,
    visibleVariant: null,
    paramValues: {},
    pendingEventSeq: null,
    pendingEvent: null,
    deliveryLease: null,
    checkpointRevision: 0,
    activeOwner: null,
    sourceMarkers: {},
    fallbackMode: null,
    annotationArtifacts: [],
    diagnostics: [],
    updatedAt: null,
  };
}

function rebuildSnapshotFromJournal(journalPath, id, assertParent) {
  let snapshot = baseSnapshot(id);
  const diagnostics = [];
  let nextSeq = 1;
  if (!fs.existsSync(journalPath)) return { snapshot, diagnostics, nextSeq };

  const lines = readStateFile(journalPath, assertParent).split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!line.trim()) continue;
    try {
      const entry = JSON.parse(line);
      if (!entry || typeof entry !== 'object') throw new Error('entry is not object');
      if (Number.isInteger(entry.seq)) nextSeq = Math.max(nextSeq, entry.seq + 1);
      snapshot = applyEvent(snapshot, entry);
    } catch (err) {
      diagnostics.push({
        error: 'journal_parse_failed',
        line: i + 1,
        message: err.message,
      });
    }
  }
  snapshot.diagnostics = [...snapshot.diagnostics, ...diagnostics];
  return { snapshot, diagnostics, nextSeq };
}

function applyEvent(snapshot, entry, inheritedDiagnostics = []) {
  const rawEvent = entry.event || entry;
  const { screenshotPath: _transientScreenshotPath, ...event } = rawEvent;
  const next = {
    ...snapshot,
    paramValues: { ...(snapshot.paramValues || {}) },
    sourceMarkers: { ...(snapshot.sourceMarkers || {}) },
    annotationArtifacts: [...(snapshot.annotationArtifacts || [])],
    diagnostics: [...(snapshot.diagnostics || [])],
    updatedAt: entry.ts || new Date().toISOString(),
  };

  if (inheritedDiagnostics.length && next.diagnostics.length === 0) {
    next.diagnostics = [...inheritedDiagnostics];
  }

  switch (event.type) {
    case 'generate':
      next.phase = 'generate_requested';
      next.pageUrl = event.pageUrl ?? next.pageUrl;
      next.expectedVariants = event.count ?? next.expectedVariants;
      next.pendingEventSeq = entry.seq ?? next.pendingEventSeq;
      next.pendingEvent = toPendingEvent(event);
      break;
    case 'variants_ready':
    case 'agent_done':
      next.phase = event.carbonize === true ? 'carbonize_required' : 'variants_ready';
      next.sourceFile = event.file ?? next.sourceFile;
      next.arrivedVariants = event.arrivedVariants ?? (next.arrivedVariants ?? next.expectedVariants);
      next.pendingEventSeq = null;
      next.pendingEvent = null;
      if (event.carbonize === true) {
        next.diagnostics.push({
          error: 'carbonize_cleanup_required',
          file: event.file || null,
          message: 'Accepted variant still has carbonize markers that must be folded into source CSS.',
        });
      }
      break;
    case 'checkpoint':
      if ((event.revision ?? 0) >= (next.checkpointRevision ?? 0)) {
        next.phase = event.phase ?? next.phase;
        next.checkpointRevision = event.revision ?? next.checkpointRevision;
        next.activeOwner = event.owner ?? next.activeOwner;
        next.arrivedVariants = event.arrivedVariants ?? next.arrivedVariants;
        next.visibleVariant = event.visibleVariant ?? next.visibleVariant;
        if (event.paramValues) next.paramValues = { ...event.paramValues };
      } else {
        next.diagnostics.push({ error: 'stale_checkpoint_ignored', revision: event.revision });
      }
      break;
    case 'accept':
    case 'accept_intent':
      next.phase = 'accept_requested';
      next.visibleVariant = Number(event.variantId ?? next.visibleVariant);
      if (event.paramValues) next.paramValues = { ...event.paramValues };
      next.pendingEventSeq = entry.seq ?? next.pendingEventSeq;
      next.pendingEvent = toPendingEvent(event);
      break;
    case 'discard':
      next.phase = 'discard_requested';
      next.pendingEventSeq = entry.seq ?? next.pendingEventSeq;
      next.pendingEvent = toPendingEvent(event);
      break;
    case 'discarded':
      next.phase = 'discarded';
      next.pendingEventSeq = null;
      next.pendingEvent = null;
      break;
    case 'complete':
      next.phase = 'completed';
      next.pendingEventSeq = null;
      next.pendingEvent = null;
      break;
    case 'agent_error':
      next.phase = 'agent_error';
      next.diagnostics.push({ error: 'agent_error', message: event.message || 'unknown agent error' });
      break;
    default:
      next.diagnostics.push({ error: 'unknown_event_type', type: event.type });
      break;
  }
  return next;
}

function toPendingEvent(event) {
  const pending = { ...event };
  delete pending.token;
  delete pending.screenshotPath;
  return pending;
}

function writeSnapshot(snapshotPath, snapshot, assertParent) {
  writeStateFile(
    snapshotPath,
    JSON.stringify(snapshot, null, 2) + '\n',
    fs.constants.O_WRONLY | fs.constants.O_CREAT | fs.constants.O_TRUNC,
    assertParent,
  );
}
