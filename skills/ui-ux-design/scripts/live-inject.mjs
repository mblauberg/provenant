// Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md.
/**
 * CLI helper: insert/remove the live variant mode script tag in the project's
 * main HTML entry point.
 *
 * On first live run, the agent generates `.impeccable/live/config.json`
 * with the project's insertion target (framework-specific). On
 * every subsequent run, this script handles insert/remove deterministically
 * with zero LLM involvement.
 *
 * Usage:
 *   node live-inject.mjs --port PORT --token TOKEN   # Insert the live script tag
 *   node live-inject.mjs --remove      # Remove the live script tag
 *   node live-inject.mjs --check       # Check whether live config exists
 */

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { resolveLiveConfigPath } from './impeccable-paths.mjs';
import {
  hasExecutableJsxMarkerAtOffset,
  frameworkTemplateContextAtOffset,
  htmlLexicalContextAtOffset,
  isOffsetInsideAstroFrontmatter,
} from './jsx-tag-scanner.mjs';
import {
  readContainedSource,
  replaceContainedSources,
} from './contained-source.mjs';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const CONFIG_PATH = resolveLiveConfigPath({ cwd: process.cwd(), scriptsDir: __dirname });
const MARKER_OPEN_TEXT = 'impeccable-live-start';
const MARKER_CLOSE_TEXT = 'impeccable-live-end';

/**
 * Hard-excluded directory patterns. These are NEVER user-facing pages and
 * matching them would silently inject tracking scripts into third-party
 * code. The user cannot turn these off via config — they are the floor.
 */
const HARD_EXCLUDES = [
  '**/node_modules/**',
  '**/.git/**',
];

export async function injectCli() {
  const args = process.argv.slice(2);

  if (args.includes('--help') || args.includes('-h')) {
    console.log(`Usage: node live-inject.mjs [options]

Insert or remove the live mode script tag in the project's HTML entry point.
Reads configuration from .impeccable/live/config.json.
Every config.files target must resolve to an existing project-relative regular file;
symlinks and multiply-linked files are rejected before any source change.

Modes:
  --port PORT   Insert script tag pointing at http://127.0.0.1:PORT/live.js
  --token TOKEN Authenticate the injected script with the current live server
  --remove      Remove the script tag (if present)
  --check       Print whether .impeccable/live/config.json exists and its content

Output (JSON):
  { ok, file, inserted|removed, config? }`);
    process.exit(0);
  }

  if (args.includes('--check')) {
    if (!fs.existsSync(CONFIG_PATH)) {
      console.log(JSON.stringify({ ok: false, error: 'config_missing', path: CONFIG_PATH }));
      process.exit(0);
    }
    let cfg;
    try {
      cfg = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf-8'));
    } catch (err) {
      console.log(JSON.stringify({ ok: false, error: 'config_invalid', message: err.message, path: CONFIG_PATH }));
      return;
    }
    try {
      validateConfig(cfg);
    } catch (err) {
      console.log(JSON.stringify({ ok: false, error: 'config_invalid', message: err.message, path: CONFIG_PATH }));
      return;
    }
    console.log(JSON.stringify({ ok: true, config: cfg, path: CONFIG_PATH }));
    return;
  }

  // Load config
  if (!fs.existsSync(CONFIG_PATH)) {
    console.error(JSON.stringify({ ok: false, error: 'config_missing', path: CONFIG_PATH }));
    process.exit(1);
  }
  const config = JSON.parse(fs.readFileSync(CONFIG_PATH, 'utf-8'));
  validateConfig(config);

  const resolvedFiles = resolveFiles(process.cwd(), config);
  // Resolve and validate the complete target set before changing any file. A
  // later escaping target must not leave earlier project files half-mutated.
  const targets = resolvedFiles.map((relFile) => ({
    relFile,
    snapshot: readContainedSource(process.cwd(), relFile, { relativeOnly: true }),
  }));

  if (args.includes('--remove')) {
    const replacements = [];
    const results = targets.map(({ relFile, snapshot }) => {
      const content = snapshot.bytes.toString('utf-8');
      const detagged = removeTag(content, config.commentSyntax, relFile);
      const updated = revertCspMeta(detagged);
      if (updated === content) return { file: relFile, removed: false, note: 'no tag present' };
      replacements.push({ snapshot, content: updated });
      return {
        file: relFile,
        removed: detagged !== content,
        cspReverted: updated !== detagged,
      };
    });
    replaceContainedSources(replacements);
    console.log(JSON.stringify({ ok: true, results }));
    return;
  }

  // Insert mode — need --port
  const portIdx = args.indexOf('--port');
  const port = portIdx !== -1 ? parseInt(args[portIdx + 1], 10) : NaN;
  if (!Number.isInteger(port) || port < 1 || port > 65535 || String(port) !== args[portIdx + 1]) {
    console.error(JSON.stringify({ ok: false, error: 'missing_port' }));
    process.exit(1);
  }
  const tokenIdx = args.indexOf('--token');
  const token = tokenIdx !== -1 ? args[tokenIdx + 1] : '';
  if (!isValidServerToken(token)) {
    console.error(JSON.stringify({ ok: false, error: 'missing_or_invalid_token' }));
    process.exit(1);
  }

  const replacements = [];
  const results = targets.map(({ relFile, snapshot }) => {
    const content = snapshot.bytes.toString('utf-8');
    const withoutOld = revertCspMeta(removeTag(content, config.commentSyntax, relFile));
    const withTag = insertTag(withoutOld, config, port, token);
    if (withTag === withoutOld) {
      return { file: relFile, error: 'insertion_point_not_found', anchor: config.insertBefore || config.insertAfter };
    }
    const updated = patchCspMeta(withTag, port);
    replacements.push({ snapshot, content: updated });
    return {
      file: relFile,
      inserted: true,
      cspPatched: updated !== withTag,
    };
  });
  const failures = results.filter((result) => result.error);
  if (failures.length > 0 || replacements.length !== targets.length) {
    console.error(JSON.stringify({ ok: false, error: 'mutation_preflight_failed', results }));
    process.exit(1);
  }
  replaceContainedSources(replacements);
  console.log(JSON.stringify({ ok: true, port, results }));
}

/**
 * Expand config.files (which may contain glob patterns) into a literal list
 * of existing file paths relative to rootDir. Literal entries pass through;
 * glob patterns are expanded via fs.globSync. HARD_EXCLUDES and config.exclude
 * are applied as filters. Duplicates are removed. Order is preserved by
 * first appearance.
 */
export function resolveFiles(rootDir, config) {
  const patterns = config.files;
  const userExcludes = Array.isArray(config.exclude) ? config.exclude : [];
  const allExcludes = [...HARD_EXCLUDES, ...userExcludes];
  const excludeRegexes = allExcludes.map(globToRegex);
  const hardExcludeRegexes = HARD_EXCLUDES.map(globToRegex);

  const isExcluded = (relPath) => excludeRegexes.some((re) => re.test(relPath));
  const isHardExcluded = (relPath) => hardExcludeRegexes.some((re) => re.test(relPath));
  const isGlob = (s) => /[*?[]/.test(s);

  const seen = new Set();
  const out = [];
  for (const pat of patterns) {
    validateProjectRelativePath(pat, 'config.files');
    if (!isGlob(pat)) {
      // Literal path — include even if it doesn't exist yet; the caller
      // reports file_not_found per-entry. User excludes do not override an
      // explicit target, but dependency metadata is a hard boundary even
      // when named literally.
      if (isHardExcluded(pat.split(path.sep).join('/'))) continue;
      if (!seen.has(pat)) {
        seen.add(pat);
        out.push(pat);
      }
      continue;
    }
    let matches;
    try {
      matches = fs.globSync(pat, { cwd: rootDir, withFileTypes: true });
    } catch {
      continue;
    }
    for (const ent of matches) {
      if (!ent.isFile || !ent.isFile()) continue;
      const abs = path.join(ent.parentPath || ent.path || rootDir, ent.name);
      const rel = path.relative(rootDir, abs).split(path.sep).join('/');
      if (isExcluded(rel)) continue;
      if (seen.has(rel)) continue;
      seen.add(rel);
      out.push(rel);
    }
  }
  // Validate every expanded path as a project-local target as well. This
  // catches glob matches reached through symlinked directories.
  for (const relFile of out) resolveProjectFileTarget(rootDir, relFile);
  return out;
}

function isContainedPath(rootPath, candidatePath) {
  const rel = path.relative(rootPath, candidatePath);
  return rel === '' || (!path.isAbsolute(rel) && rel !== '..' && !rel.startsWith(`..${path.sep}`));
}

function validateProjectRelativePath(value, field) {
  if (value.includes('\0')) throw new Error(`${field} entries must not contain NUL bytes`);
  const portable = value.replaceAll('\\', '/');
  if (path.isAbsolute(value) || path.posix.isAbsolute(portable) || path.win32.isAbsolute(value)) {
    throw new Error(`${field} entries must be project-relative paths`);
  }
  if (portable.split('/').includes('..')) {
    throw new Error(`${field} entries must not traverse an ancestor directory`);
  }
}

/**
 * Resolve a literal file target without permitting lexical or symlink escape.
 * Missing files are allowed so the CLI can retain its per-file
 * `file_not_found` result, but their nearest existing ancestor must still be
 * inside the project root.
 */
function resolveProjectFileTarget(rootDir, relFile) {
  validateProjectRelativePath(relFile, 'config.files');
  const rootAbs = path.resolve(rootDir);
  const rootReal = fs.realpathSync.native(rootAbs);
  const candidateAbs = path.resolve(rootAbs, relFile);
  if (!isContainedPath(rootAbs, candidateAbs)) {
    throw new Error(`config.files target escapes project root: ${relFile}`);
  }

  let existing = candidateAbs;
  while (!fs.existsSync(existing)) {
    const parent = path.dirname(existing);
    if (parent === existing) break;
    existing = parent;
  }
  const existingReal = fs.realpathSync.native(existing);
  if (!isContainedPath(rootReal, existingReal)) {
    throw new Error(`config.files target escapes project root through a symlink: ${relFile}`);
  }

  if (!fs.existsSync(candidateAbs)) return candidateAbs;
  const candidateReal = fs.realpathSync.native(candidateAbs);
  if (!isContainedPath(rootReal, candidateReal)) {
    throw new Error(`config.files target escapes project root through a symlink: ${relFile}`);
  }
  return candidateReal;
}

/**
 * Convert a glob pattern to a RegExp. Supports:
 *   **  → any number of path segments (including zero)
 *   *   → any chars except `/`
 *   ?   → any single char except `/`
 * Paths are normalized to forward slashes before matching.
 */
function globToRegex(pattern) {
  let re = '';
  let i = 0;
  while (i < pattern.length) {
    const c = pattern[i];
    if (c === '*') {
      if (pattern[i + 1] === '*') {
        // ** — any number of segments, including zero. Handle the common
        // **/ and /** forms so `a/**/b` matches `a/b` as well as `a/x/y/b`.
        if (pattern[i + 2] === '/') {
          re += '(?:.*/)?';
          i += 3;
        } else {
          re += '.*';
          i += 2;
        }
      } else {
        re += '[^/]*';
        i += 1;
      }
    } else if (c === '?') {
      re += '[^/]';
      i += 1;
    } else if (/[.+^${}()|[\]\\]/.test(c)) {
      re += '\\' + c;
      i += 1;
    } else {
      re += c;
      i += 1;
    }
  }
  return new RegExp('^' + re + '$');
}

// ---------------------------------------------------------------------------
// Core operations
// ---------------------------------------------------------------------------

function validateConfig(cfg) {
  if (!cfg || typeof cfg !== 'object') throw new Error('config.json must be an object');
  if (!Array.isArray(cfg.files) || cfg.files.length === 0) {
    throw new Error('config.files (non-empty string array) required');
  }
  if (!cfg.files.every((f) => typeof f === 'string' && f.length > 0)) {
    throw new Error('config.files must contain only non-empty strings');
  }
  for (const file of cfg.files) validateProjectRelativePath(file, 'config.files');
  if (cfg.exclude !== undefined) {
    if (!Array.isArray(cfg.exclude)) {
      throw new Error('config.exclude, if present, must be a string array');
    }
    if (!cfg.exclude.every((f) => typeof f === 'string' && f.length > 0)) {
      throw new Error('config.exclude must contain only non-empty strings');
    }
  }
  if (typeof cfg.insertBefore !== 'string' && typeof cfg.insertAfter !== 'string') {
    throw new Error('config.insertBefore or config.insertAfter (string) required');
  }
  if (cfg.commentSyntax !== 'html' && cfg.commentSyntax !== 'jsx') {
    throw new Error("config.commentSyntax must be 'html' or 'jsx'");
  }
  if (cfg.cspChecked !== undefined && typeof cfg.cspChecked !== 'boolean') {
    throw new Error("config.cspChecked, if present, must be a boolean");
  }
}

function commentOpen(syntax) { return syntax === 'jsx' ? '{/*' : '<!--'; }
function commentClose(syntax) { return syntax === 'jsx' ? '*/}' : '-->'; }

function isValidServerToken(token) {
  return typeof token === 'string'
    && /^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(token);
}

function buildTagBlock(syntax, port, token) {
  const open = commentOpen(syntax);
  const close = commentClose(syntax);
  return (
    open + ' ' + MARKER_OPEN_TEXT + ' ' + close + '\n' +
    '<script src="http://127.0.0.1:' + port + '/live.js?token=' + encodeURIComponent(token) + '"></script>\n' +
    open + ' ' + MARKER_CLOSE_TEXT + ' ' + close + '\n'
  );
}

function insertTag(content, config, port, token) {
  const block = buildTagBlock(config.commentSyntax, port, token);
  // insertBefore: match the LAST occurrence. Anchors like `</body>` naturally
  // belong at the end, and the same literal can appear earlier in code blocks
  // within rendered documentation pages.
  if (config.insertBefore) {
    const idx = content.lastIndexOf(config.insertBefore);
    if (idx === -1) return content;
    return content.slice(0, idx) + block + content.slice(idx);
  }
  // insertAfter: match the FIRST occurrence — typical anchors like `<head>` or
  // `<body>` open near the top of the document.
  const idx = content.indexOf(config.insertAfter);
  if (idx === -1) return content;
  const after = idx + config.insertAfter.length;
  // Preserve a single trailing newline if the anchor didn't end with one
  const hasNewline = content[after] === '\n';
  const prefix = hasNewline ? content.slice(0, after + 1) : content.slice(0, after) + '\n';
  const suffixStart = hasNewline ? after + 1 : after;
  return prefix + block + content.slice(suffixStart);
}

/**
 * Remove the live script block. Matches either HTML or JSX comment markers
 * regardless of config (so stale tags from a wrong config can still be cleaned).
 *
 * Indent-preserving: captures any whitespace immediately preceding the opener
 * marker and re-emits it in place of the removed block. `insertTag` inserted
 * the block *after* the original line's indent and *before* the anchor (e.g.
 * `</body>`), which moved the indent onto the opener line and left the anchor
 * unindented. Replacing the whole block (plus its trailing newline) with just
 * the captured indent hands the indent back to the anchor that follows.
 */
function removeTag(content, _syntax, filePath = '') {
  const extension = path.extname(filePath).toLowerCase();
  const isFramework = ['.astro', '.svelte', '.vue'].includes(extension);
  const patterns = [
    {
      syntax: 'jsx',
      pattern: /([ \t]*)\{\/\* impeccable-live-start \*\/\}\r?\n[ \t]*<script src="http:\/\/127\.0\.0\.1:([0-9]{1,5})\/live\.js\?token=([0-9a-f-]+)"><\/script>\r?\n[ \t]*\{\/\* impeccable-live-end \*\/\}[ \t]*(?:\r?\n|$)/gi,
    },
    {
      syntax: 'html',
      pattern: /([ \t]*)<!-- impeccable-live-start -->\r?\n[ \t]*<script src="http:\/\/127\.0\.0\.1:([0-9]{1,5})\/live\.js\?token=([0-9a-f-]+)"><\/script>\r?\n[ \t]*<!-- impeccable-live-end -->[ \t]*(?:\r?\n|$)/gi,
    },
  ];
  const matches = patterns.flatMap(({ syntax, pattern }) => (
    [...content.matchAll(pattern)].filter((match) => {
      const port = Number(match[2]);
      if (!Number.isInteger(port) || port < 1 || port > 65535 || !isValidServerToken(match[3])) {
        return false;
      }
      const markerOffset = match.index + match[1].length;
      if (syntax === 'jsx') {
        return hasExecutableJsxMarkerAtOffset(
          content,
          markerOffset,
          '{/* impeccable-live-start */}'.length,
        );
      }
      if (isFramework) {
        return !isOffsetInsideAstroFrontmatter(content, markerOffset)
          && frameworkTemplateContextAtOffset(content, markerOffset) === 'markup';
      }
      return htmlLexicalContextAtOffset(content, markerOffset) === 'markup';
    })
  ));
  if (matches.length === 0) return content;
  if (matches.length !== 1) {
    const error = new Error('Multiple executable live marker blocks found');
    error.code = 'live_marker_ambiguous';
    throw error;
  }
  const [match] = matches;
  return content.slice(0, match.index) + match[1] + content.slice(match.index + match[0].length);
}

// ---------------------------------------------------------------------------
// Content-Security-Policy meta-tag patcher
//
// When the user's HTML carries `<meta http-equiv="Content-Security-Policy">`,
// the cross-origin load of /live.js (and the SSE/POST connection back to
// 127.0.0.1:PORT) is blocked unless the CSP explicitly allows that origin.
//
// On insert: append `http://127.0.0.1:PORT` to `script-src` and `connect-src`,
// and stash the original `content` value in a `data-impeccable-csp-original`
// attribute (base64) so revert is exact.
//
// On remove: detect the marker attribute, decode it, restore the original
// content value verbatim, drop the marker.
//
// Header-based CSP (Next.js headers, Nuxt routeRules, SvelteKit kit.csp,
// shared helpers) is NOT patched here — those need framework-specific config
// edits and are handled via the existing detect-csp.mjs reference output.
// Only the in-source meta-tag form gets the auto-patch.
// ---------------------------------------------------------------------------

const CSP_MARKER_ATTR = 'data-impeccable-csp-original';

function decodeCanonicalUtf8Base64(value) {
  const canonicalBase64 = /^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/;
  try {
    if (!canonicalBase64.test(value)) throw new Error('non-canonical base64');
    const bytes = Buffer.from(value, 'base64');
    if (bytes.toString('base64') !== value) throw new Error('non-canonical base64');
    const decoded = bytes.toString('utf8');
    if (!Buffer.from(decoded, 'utf8').equals(bytes)) throw new Error('invalid UTF-8');
    return decoded;
  } catch (cause) {
    const error = new Error('Malformed Impeccable CSP restoration marker', { cause });
    error.code = 'csp_marker_invalid';
    throw error;
  }
}

function findCspMetaTags(content) {
  const out = [];
  const tagRe = /<meta\s+([^>]*?)\/?>/gis;
  let m;
  while ((m = tagRe.exec(content)) !== null) {
    const attrs = m[1];
    if (!/(http-equiv|httpEquiv)\s*=\s*(['"])Content-Security-Policy\2/i.test(attrs)) continue;
    out.push({ start: m.index, end: m.index + m[0].length, full: m[0], attrs });
  }
  return out;
}

function getAttr(attrs, name) {
  const re = new RegExp(`\\b${name}\\s*=\\s*(['"])([\\s\\S]*?)\\1`, 'i');
  const m = attrs.match(re);
  return m ? { quote: m[1], value: m[2], full: m[0] } : null;
}

function appendOriginToDirective(csp, directive, origin) {
  const re = new RegExp(`(^|;)(\\s*)(${directive})\\s+([^;]*)`, 'i');
  const m = csp.match(re);
  if (m) {
    const tokens = m[4].trim().split(/\s+/);
    if (tokens.includes(origin)) return csp;
    return csp.replace(re, `${m[1]}${m[2]}${m[3]} ${[...tokens, origin].join(' ')}`);
  }
  // Directive missing — add it. Use 'self' + origin so we don't inadvertently
  // narrow the policy compared to the default-src fallback (most users with
  // an explicit CSP have 'self' there).
  return csp.trim().replace(/;?\s*$/, '') + `; ${directive} 'self' ${origin}`;
}

export function patchCspMeta(content, port) {
  const tags = findCspMetaTags(content);
  if (tags.length === 0) return content;
  const origin = `http://127.0.0.1:${port}`;

  // Walk last-to-first so prior splices don't invalidate later indices.
  let result = content;
  for (let i = tags.length - 1; i >= 0; i--) {
    const tag = tags[i];
    const attrs = tag.attrs;
    if (getAttr(attrs, CSP_MARKER_ATTR)) continue; // already patched
    const contentAttr = getAttr(attrs, 'content');
    if (!contentAttr) continue;

    const original = contentAttr.value;
    let patched = original;
    patched = appendOriginToDirective(patched, 'script-src', origin);
    patched = appendOriginToDirective(patched, 'connect-src', origin);
    // The shader overlay during 'generating' creates a screenshot via
    // URL.createObjectURL, producing a `blob:` URL — img-src 'self' rejects
    // those. Add `blob:` so the overlay doesn't throw a CSP violation.
    patched = appendOriginToDirective(patched, 'img-src', 'blob:');
    if (patched === original) continue;

    const newContentAttr = `content=${contentAttr.quote}${patched}${contentAttr.quote}`;
    const marker = `${CSP_MARKER_ATTR}="${Buffer.from(original, 'utf-8').toString('base64')}"`;
    // The tagRe captures any whitespace between the last attribute and the
    // closing `/>` as part of `attrs`. Naively appending ` ${marker}` after
    // a replace would land it BEFORE that trailing space, leaving a double
    // space inside attrs and clobbering the space before `/>`. Split off
    // the trailing whitespace, splice the marker into the attribute body,
    // and re-append the original trailing whitespace so a self-closing
    // `<meta … />` round-trips byte-for-byte.
    const trailingWs = (attrs.match(/[ \t]*$/) || [''])[0];
    const attrsBody = attrs.slice(0, attrs.length - trailingWs.length);
    const newAttrs = attrsBody.replace(contentAttr.full, newContentAttr) + ' ' + marker + trailingWs;
    const newTag = tag.full.replace(attrs, newAttrs);

    result = result.slice(0, tag.start) + newTag + result.slice(tag.end);
  }
  return result;
}

export function revertCspMeta(content) {
  const tags = findCspMetaTags(content);
  if (tags.length === 0) return content;

  let result = content;
  for (let i = tags.length - 1; i >= 0; i--) {
    const tag = tags[i];
    const origAttr = getAttr(tag.attrs, CSP_MARKER_ATTR);
    if (!origAttr) continue;
    const contentAttr = getAttr(tag.attrs, 'content');
    if (!contentAttr) continue;

    const originalValue = decodeCanonicalUtf8Base64(origAttr.value);

    const newContentAttr = `content=${contentAttr.quote}${originalValue}${contentAttr.quote}`;
    let newAttrs = tag.attrs.replace(contentAttr.full, newContentAttr);
    // Drop the exact marker text and its generated separator. Base64 can
    // contain regexp metacharacters, so this must remain a literal splice.
    const markerIndex = newAttrs.indexOf(origAttr.full);
    if (markerIndex === -1) continue;
    const markerStart = markerIndex > 0 && newAttrs[markerIndex - 1] === ' '
      ? markerIndex - 1
      : markerIndex;
    newAttrs = newAttrs.slice(0, markerStart) + newAttrs.slice(markerIndex + origAttr.full.length);
    const newTag = tag.full.replace(tag.attrs, newAttrs);

    result = result.slice(0, tag.start) + newTag + result.slice(tag.end);
  }
  return result;
}

// ---------------------------------------------------------------------------
// Auto-execute
// ---------------------------------------------------------------------------

const _running = process.argv[1];
if (_running?.endsWith('live-inject.mjs') || _running?.endsWith('live-inject.mjs/')) {
  injectCli().catch((error) => {
    console.error(JSON.stringify({
      ok: false,
      error: error.code || 'inject_failed',
      message: error.message,
      ...(error.rollbackErrors ? { rollbackErrors: error.rollbackErrors } : {}),
    }));
    process.exit(1);
  });
}

export { insertTag, removeTag, validateConfig, buildTagBlock };
// patchCspMeta + revertCspMeta are exported above where they're defined.
