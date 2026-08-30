// Modified for Provenant.
/**
 * Browser-side durable session helpers for Impeccable live mode.
 *
 * Kept separate from live-browser.js so recovery state can be tested without
 * booting the full overlay UI. Served before live-browser.js and attached to
 * window.__IMPECCABLE_LIVE_SESSION__.
 */
(function (root) {
  'use strict';

  const RETRY_ACTIONS = new Set([
    'impeccable', 'bolder', 'quieter', 'distill', 'polish', 'typeset',
    'colorize', 'layout', 'adapt', 'animate', 'delight', 'overdrive',
  ]);

  function cloneJson(value) {
    try { return JSON.parse(JSON.stringify(value)); } catch { return null; }
  }

  function buildRetryGenerationEvent({ id, intent } = {}) {
    if (!/^[0-9a-f]{8}$/.test(id || '')
      || !intent
      || !RETRY_ACTIONS.has(intent.action)
      || !Number.isInteger(intent.count)
      || intent.count < 1
      || intent.count > 8
      || typeof intent.pageUrl !== 'string'
      || !intent.element
      || typeof intent.element.outerHTML !== 'string'
      || !intent.element.outerHTML
      || (intent.freeformPrompt !== undefined && typeof intent.freeformPrompt !== 'string')
      || (intent.comments !== undefined && !Array.isArray(intent.comments))
      || (intent.strokes !== undefined && !Array.isArray(intent.strokes))) return null;
    const clonedElement = cloneJson(intent.element);
    const clonedComments = intent.comments === undefined ? undefined : cloneJson(intent.comments);
    const clonedStrokes = intent.strokes === undefined ? undefined : cloneJson(intent.strokes);
    if (!clonedElement
      || (intent.comments !== undefined && !clonedComments)
      || (intent.strokes !== undefined && !clonedStrokes)) return null;
    const event = {
      type: 'generate',
      id,
      action: intent.action,
      count: intent.count,
      pageUrl: intent.pageUrl,
      element: clonedElement,
    };
    if (intent.freeformPrompt !== undefined) event.freeformPrompt = intent.freeformPrompt;
    if (clonedComments !== undefined) event.comments = clonedComments;
    if (clonedStrokes !== undefined) event.strokes = clonedStrokes;
    return event;
  }

  function nextRovingTabIndex(key, currentIndex, tabCount) {
    if (!Number.isInteger(currentIndex)
      || !Number.isInteger(tabCount)
      || tabCount < 1
      || currentIndex < 0
      || currentIndex >= tabCount) return null;
    if (key === 'Home') return 0;
    if (key === 'End') return tabCount - 1;
    if (key === 'ArrowRight') return (currentIndex + 1) % tabCount;
    if (key === 'ArrowLeft') return (currentIndex - 1 + tabCount) % tabCount;
    return null;
  }

  function safeMarkdownHref(value) {
    const href = String(value ?? '').trim();
    if (!href) return null;
    // Browsers ignore ASCII control whitespace while parsing a scheme, so use
    // the same compact form when deciding whether an explicit scheme is safe.
    const compact = href.replace(/[\u0000-\u0020\u007f]+/g, '');
    const scheme = compact.match(/^([a-z][a-z0-9+.-]*):/i);
    if (!scheme) return href;
    return ['http', 'https', 'mailto', 'tel'].includes(scheme[1].toLowerCase())
      ? href
      : null;
  }

  function createLiveBrowserSessionState({ prefix, storage, idFactory }) {
    if (!prefix) throw new Error('prefix required');
    const store = storage || root.localStorage;
    const makeId = idFactory || function () { return Math.random().toString(16).slice(2, 10); };
    const sessionKey = prefix + '-session';
    const handledKey = sessionKey + '-handled';
    const scrollKey = sessionKey + '-scroll';
    let checkpointRevision = 0;
    const owner = makeId();

    function safeRead(key) {
      try { return store.getItem(key); } catch { return null; }
    }

    function safeWrite(key, value) {
      try { store.setItem(key, value); } catch { /* quota exceeded or private mode */ }
    }

    function safeRemove(key) {
      try { store.removeItem(key); } catch { /* unavailable storage */ }
    }

    function loadSession() {
      try {
        const raw = safeRead(sessionKey);
        if (!raw) return null;
        const parsed = JSON.parse(raw);
        if (Number.isInteger(parsed.checkpointRevision)) {
          checkpointRevision = Math.max(checkpointRevision, parsed.checkpointRevision);
        }
        return parsed;
      } catch { return null; }
    }

    function saveSession(session) {
      if (!session || !session.id) return;
      const payload = {
        ...session,
        checkpointRevision,
      };
      safeWrite(sessionKey, JSON.stringify(payload));
    }

    function clearSession() {
      safeRemove(sessionKey);
    }

    function nextCheckpointRevision() {
      checkpointRevision += 1;
      const existing = loadSession();
      if (existing?.id) saveSession(existing);
      return checkpointRevision;
    }

    function seedCheckpointRevision(value) {
      if (Number.isInteger(value)) checkpointRevision = Math.max(checkpointRevision, value);
      return checkpointRevision;
    }

    function currentCheckpointRevision() {
      return checkpointRevision;
    }

    function markHandled(id) {
      if (!id) return;
      safeWrite(handledKey, id);
    }

    function isHandled(id) {
      return !!id && safeRead(handledKey) === id;
    }

    function clearHandled() {
      safeRemove(handledKey);
    }

    function writeScrollY(y) {
      safeWrite(scrollKey, String(y));
    }

    function readScrollY() {
      const raw = safeRead(scrollKey);
      if (raw == null) return null;
      const n = parseFloat(raw);
      return isFinite(n) ? n : null;
    }

    function clearScrollY() {
      safeRemove(scrollKey);
    }

    return {
      owner,
      sessionKey,
      handledKey,
      scrollKey,
      saveSession,
      loadSession,
      clearSession,
      nextCheckpointRevision,
      seedCheckpointRevision,
      currentCheckpointRevision,
      markHandled,
      isHandled,
      clearHandled,
      writeScrollY,
      readScrollY,
      clearScrollY,
      buildRetryGenerationEvent,
    };
  }

  root.__IMPECCABLE_LIVE_SESSION__ = {
    createLiveBrowserSessionState,
    buildRetryGenerationEvent,
    nextRovingTabIndex,
    safeMarkdownHref,
  };
})(typeof window !== 'undefined' ? window : globalThis);
