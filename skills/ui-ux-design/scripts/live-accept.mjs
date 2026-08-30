// Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md.
/**
 * CLI helper: deterministic accept/discard of variant sessions.
 *
 * Usage:
 *   node live-accept.mjs --id SESSION_ID --discard
 *   node live-accept.mjs --id SESSION_ID --variant N
 *
 * For discard: removes the entire variant wrapper and restores the original.
 * For accept: replaces the wrapper with the chosen variant's content. If the
 * session had a colocated <style> block, it's preserved with carbonize markers
 * for a background agent to integrate into the project's CSS.
 *
 * Output: JSON to stdout.
 */

import fs from 'node:fs';
import path from 'node:path';
import { isGeneratedFile } from './is-generated.mjs';
import { readContainedSource, replaceContainedSource } from './contained-source.mjs';
import {
  findJsxSubtree,
  findMatchingJsxTag,
  frameworkTemplateContextAtOffset,
  hasExecutableJsxMarkerAtOffset,
  htmlLexicalContextAtOffset,
  isOffsetInsideAstroFrontmatter,
  scanJsxTags,
} from './jsx-tag-scanner.mjs';

const EXTENSIONS = ['.html', '.jsx', '.tsx', '.vue', '.svelte', '.astro'];
const SESSION_ID_PATTERN = /^[A-Za-z0-9_-]{1,128}$/;
const VARIANT_ID_PATTERN = /^[1-8]$/;

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

export async function acceptCli() {
  const args = process.argv.slice(2);

  if (args.includes('--help') || args.includes('-h')) {
    console.log(`Usage: node live-accept.mjs [options]

Deterministic accept/discard for live variant sessions.

Modes:
  --discard          Remove variants, restore original
  --variant N        Accept variant N, discard the rest

Required:
  --id SESSION_ID    Session ID of the variant wrapper

Output (JSON):
  { handled, file, carbonize }`);
    process.exit(0);
  }

  const id = argVal(args, '--id');
  const variantNum = argVal(args, '--variant');
  const paramValuesRaw = argVal(args, '--param-values');
  const isDiscard = args.includes('--discard');

  if (!id) { console.error('Missing --id'); process.exit(1); }
  if (!isDiscard && !variantNum) { console.error('Need --discard or --variant N'); process.exit(1); }
  if (!SESSION_ID_PATTERN.test(id)) {
    console.error(JSON.stringify({ error: 'invalid_session_id' }));
    process.exit(1);
  }
  if (!isDiscard && !VARIANT_ID_PATTERN.test(variantNum)) {
    console.error(JSON.stringify({ error: 'invalid_variant_id' }));
    process.exit(1);
  }

  let paramValues = null;
  if (paramValuesRaw) {
    try { paramValues = JSON.parse(paramValuesRaw); }
    catch { paramValues = null; } // malformed blob: skip the comment rather than failing the accept
  }

  // Find the file containing this session's markers
  const found = findSessionFile(id, process.cwd());
  if (!found) {
    console.log(JSON.stringify({ handled: false, error: 'Session markers not found for id: ' + id }));
    process.exit(0);
  }

  const { file: targetFile, lines, snapshot } = found;
  const relFile = path.relative(process.cwd(), targetFile);

  // Bail if the session lives in a generated file. The agent manually wrote
  // the wrapper there for preview, and is responsible for writing the
  // accepted variant to true source (or cleaning up on discard). See
  // "Handle fallback" in live.md.
  if (isGeneratedFile(targetFile, { cwd: process.cwd() })) {
    console.log(JSON.stringify({
      handled: false,
      mode: 'fallback',
      file: relFile,
      hint: 'Session is in a generated file. Persist the accepted variant in source; do not rely on this script.',
    }));
    process.exit(0);
  }

  if (isDiscard) {
    const result = handleDiscard(id, lines, targetFile, snapshot);
    console.log(JSON.stringify({ handled: true, file: relFile, carbonize: false, ...result }));
  } else {
    const result = handleAccept(id, variantNum, lines, targetFile, paramValues, snapshot);
    // Single-line attention-grabber when cleanup is required. The full
    // checklist lives in references/live.md (loaded once per
    // session); repeating it per-event would waste tokens.
    if (result.carbonize) {
      result.todo = 'REQUIRED before next poll: carbonize cleanup in ' + relFile + '. See references/live.md "Required after accept".';
    }
    console.log(JSON.stringify({ handled: true, file: relFile, ...result }));
  }
}

// ---------------------------------------------------------------------------
// Discard
// ---------------------------------------------------------------------------

function handleDiscard(id, lines, targetFile, snapshot) {
  const commentSyntax = detectCommentSyntax(targetFile);
  const block = findMarkerBlock(id, lines, commentSyntax, targetFile);
  if (!block) return { handled: false, error: 'Session markers are missing or ambiguous' };

  const original = extractOriginal(lines, block, id);
  if (original === null) return { handled: false, error: 'Original session content is missing' };
  const isJsx = commentSyntax.open === '{/*';
  let replaceRange;
  try {
    replaceRange = expandReplaceRange(id, block, lines, isJsx);
  } catch (error) {
    return { handled: false, error: error.code || 'Session wrapper is invalid' };
  }

  // Restore at the line we're actually replacing FROM, not the marker line.
  // For JSX wrappers the marker comments live INSIDE the outer `<div>`, so
  // `block.start` sits 2 spaces deeper than the original element. Using that
  // as the deindent base would push the restored content 2 spaces too far
  // right on every JSX/TSX session. `replaceRange.start` is the outer wrapper
  // line, which is at the original element's indent for both HTML and JSX.
  const indent = lines[replaceRange.start].match(/^(\s*)/)[1];
  const restored = deindentContent(original, indent);
  const source = lines.join('\n');
  const replacement = joinReplacementLines(restored, source);
  replaceContainedSource(
    snapshot,
    source.slice(0, replaceRange.startOffset) + replacement + source.slice(replaceRange.endOffset),
  );
  return {};
}

// ---------------------------------------------------------------------------
// Accept
// ---------------------------------------------------------------------------

function handleAccept(id, variantNum, lines, targetFile, paramValues, snapshot) {
  const commentSyntax = detectCommentSyntax(targetFile);
  const block = findMarkerBlock(id, lines, commentSyntax, targetFile);
  if (!block) return { handled: false, error: 'Session markers are missing or ambiguous' };
  if (extractOriginal(lines, block, id) === null) {
    return { handled: false, error: 'Original session content is missing' };
  }
  const isJsx = commentSyntax.open === '{/*';
  // Anchor indent on the line we're replacing FROM (the outer wrapper),
  // not on `block.start` — for JSX that's the marker comment 2 spaces
  // deeper than the original element. See handleDiscard for the full
  // rationale.
  let replaceRange;
  try {
    replaceRange = expandReplaceRange(id, block, lines, isJsx);
  } catch (error) {
    return { handled: false, error: error.code || 'Session wrapper is invalid' };
  }
  const indent = lines[replaceRange.start].match(/^(\s*)/)[1];

  // Extract the chosen variant's inner content
  const variantContent = extractVariant(lines, block, variantNum, id);
  if (!variantContent) return { handled: false, error: 'Variant ' + variantNum + ' not found' };

  // Extract CSS block if present
  const cssContent = extractCss(lines, block, id);

  // Check if carbonizing is needed:
  // - CSS block exists, OR
  // - variant HTML contains helper classes/attributes that need cleanup
  const variantText = variantContent.join('\n');
  const hasHelperAttrs = variantText.includes('data-impeccable-variant');
  const needsCarbonize = !!(cssContent || hasHelperAttrs);

  // Build the replacement
  const restored = deindentContent(variantContent, indent);
  const replacement = [];

  if (cssContent) {
    replacement.push(indent + commentSyntax.open + ' impeccable-carbonize-start ' + id + ' ' + commentSyntax.close);
    // JSX targets need the CSS body wrapped in a template literal so that the
    // `{` and `}` in CSS rules don't get parsed as JSX expressions.
    replacement.push(indent + '<style data-impeccable-css="' + id + '">' + (isJsx ? '{`' : ''));
    // Re-indent CSS content to match
    for (const cssLine of cssContent) {
      replacement.push(indent + cssLine.trimStart());
    }
    replacement.push(indent + (isJsx ? '`}</style>' : '</style>'));
    if (paramValues && Object.keys(paramValues).length > 0) {
      // Preserve the user's knob positions for the carbonize-cleanup agent
      // to bake into the final CSS when it collapses scoped rules. Keep the
      // JSON out of comment syntax: values may contain HTML or JSX comment
      // terminators, so the payload is encoded as UTF-8 base64.
      const encodedParamValues = Buffer.from(JSON.stringify(paramValues), 'utf8').toString('base64');
      replacement.push(indent + commentSyntax.open + ' impeccable-param-values ' + id + ': base64:' + encodedParamValues + ' ' + commentSyntax.close);
    }
    replacement.push(indent + commentSyntax.open + ' impeccable-carbonize-end ' + id + ' ' + commentSyntax.close);
  }

  // Keep the `@scope ([data-impeccable-variant="N"])` selectors in the
  // carbonize CSS block working visually by re-wrapping the accepted content
  // in a data-impeccable-variant="N" div with `display: contents` (so layout
  // isn't affected). The carbonize agent strips this attribute + wrapper when
  // it moves the CSS to a proper stylesheet.
  //
  // Style attribute syntax has to follow the host file's flavor — JSX files
  // need the object form, otherwise React 19 throws "Failed to set indexed
  // property [0] on CSSStyleDeclaration" while parsing the string char-by-char.
  if (cssContent) {
    const styleAttr = isJsx ? "style={{ display: 'contents' }}" : 'style="display: contents"';
    replacement.push(indent + '<div data-impeccable-variant="' + variantNum + '" ' + styleAttr + '>');
    replacement.push(...restored);
    replacement.push(indent + '</div>');
  } else {
    replacement.push(...restored);
  }

  const source = lines.join('\n');
  replaceContainedSource(
    snapshot,
    source.slice(0, replaceRange.startOffset)
      + joinReplacementLines(replacement, source)
      + source.slice(replaceRange.endOffset),
  );

  return { carbonize: needsCarbonize };
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/**
 * Find the start/end marker lines for a session.
 * Returns { start, end } (0-indexed line numbers) or null.
 */
function markerText(kind, id, commentSyntax) {
  return `${commentSyntax.open} impeccable-variants-${kind} ${id} ${commentSyntax.close}`;
}

function findMarkerBlock(
  id,
  lines,
  commentSyntax = { open: '<!--', close: '-->' },
  filePath = '',
) {
  const startText = markerText('start', id, commentSyntax);
  const endText = markerText('end', id, commentSyntax);
  const starts = [];
  const ends = [];
  const source = lines.join('\n');
  const offsets = buildLineOffsets(lines);
  for (let index = 0; index < lines.length; index += 1) {
    for (const [marker, found] of [
      [startText, starts],
      [endText, ends],
    ]) {
      let column = lines[index].indexOf(marker);
      while (column !== -1) {
        const offset = offsets[index] + column;
        if (isExecutableMarker(source, offset, marker.length, filePath, commentSyntax)) {
          found.push(index);
        }
        column = lines[index].indexOf(marker, column + marker.length);
      }
    }
  }
  if (starts.length !== 1 || ends.length !== 1 || starts[0] >= ends[0]) return null;
  return { start: starts[0], end: ends[0] };
}

function isExecutableMarker(source, offset, length, filePath, syntax) {
  if (syntax.open === '{/*') return hasExecutableJsxMarkerAtOffset(source, offset, length);
  const extension = path.extname(filePath).toLowerCase();
  if (['.astro', '.svelte', '.vue'].includes(extension)) {
    return !isOffsetInsideAstroFrontmatter(source, offset)
      && frameworkTemplateContextAtOffset(source, offset) === 'markup';
  }
  return htmlLexicalContextAtOffset(source, offset) === 'markup';
}

/**
 * Compute the line range to REPLACE (vs. just the marker range to extract
 * from). For JSX/TSX wrappers, live-wrap places the marker comments INSIDE
 * the `<div data-impeccable-variants="ID">` outer wrapper so the picked
 * element's JSX slot keeps a single child — a Fragment `<></>` would have
 * solved the multi-sibling case but failed inside `asChild` / cloneElement
 * parents with "Invalid prop supplied to React.Fragment".
 *
 * That means the marker block is enclosed by the wrapper `<div>` opener
 * (with `data-impeccable-variants="ID"`) and its matching `</div>`. We
 * walk back to the opener and forward to the closer so accept/discard
 * remove the entire scaffold, not just the inner markers.
 *
 * Marker lines themselves stay where they were so extractOriginal /
 * extractVariant / extractCss continue to walk the same range.
 */
function expandReplaceRange(id, block, lines, isJsx) {
  const wrapperNeedle = `data-impeccable-variants="${id}"`;
  const source = lines.join('\n');
  const offsets = buildLineOffsets(lines);
  if (!isJsx) {
    const blockText = lines.slice(block.start, block.end + 1).join('\n');
    if (blockText.split(wrapperNeedle).length !== 2) {
      const error = new Error('Session wrapper does not match its requested id');
      error.code = 'session_structure_invalid';
      throw error;
    }
    const openMarker = `<!-- impeccable-variants-start ${id} -->`;
    const closeMarker = `<!-- impeccable-variants-end ${id} -->`;
    const openIndex = lines[block.start].indexOf(openMarker);
    const closeIndex = lines[block.end].indexOf(closeMarker);
    if (openIndex === -1 || closeIndex === -1) {
      const error = new Error('Session marker offsets are invalid');
      error.code = 'session_structure_invalid';
      throw error;
    }
    const exactStart = offsets[block.start] + openIndex;
    return {
      start: block.start,
      end: block.end,
      startOffset: includeLeadingLineIndent(source, exactStart),
      endOffset: offsets[block.end] + closeIndex + closeMarker.length,
    };
  }
  const { opener, closing } = findJsxSubtree(source, (tag) => tag.name === 'div'
    && tag.raw.includes(wrapperNeedle));
  const start = source.slice(0, opener.start).split('\n').length - 1;
  const end = source.slice(0, closing.end).split('\n').length - 1;
  if (end < block.end) {
    const error = new Error('JSX variant wrapper closes before its marker block');
    error.code = 'jsx_scan_unbalanced';
    throw error;
  }
  return {
    start,
    end,
    startOffset: includeLeadingLineIndent(source, opener.start),
    endOffset: closing.end,
  };
}

function buildLineOffsets(lines) {
  const offsets = [];
  let offset = 0;
  for (const line of lines) {
    offsets.push(offset);
    offset += line.length + 1;
  }
  return offsets;
}

function includeLeadingLineIndent(source, offset) {
  const lineStart = source.lastIndexOf('\n', offset - 1) + 1;
  return /^\s*$/.test(source.slice(lineStart, offset)) ? lineStart : offset;
}

function joinReplacementLines(lines, source) {
  const newline = source.includes('\r\n') ? '\r\n' : '\n';
  return lines
    .map((line) => (newline === '\r\n' && line.endsWith('\r') ? line.slice(0, -1) : line))
    .join(newline);
}

/**
 * Join wrapper lines into a single string with `<style>` elements removed so
 * marker matching and div-depth tracking aren't confused by:
 *   - CSS `@scope ([data-impeccable-variant="N"])` strings that look like the
 *     HTML marker we're searching for
 *   - JSX self-closing `<style ... />` (no separate `</style>` to close on)
 *   - Same-line `<style>…</style>` blocks
 *   - Multi-line `<style>\n…\n</style>` blocks
 */
function stripStyleAndJoin(lines, block, id) {
  const out = [];
  let inStyle = false;
  const styleAttr = `data-impeccable-css="${id}"`;
  for (let i = block.start; i <= block.end; i++) {
    let line = lines[i];

    if (!inStyle) {
      // Strip any complete <style> elements on this line (self-closed or
      // same-line-closed), including their body content.
      line = line
        .replace(/<style\b[^>]*>[\s\S]*?<\/style\s*>/g, (match) => (
          match.includes(styleAttr) ? '' : match
        ))
        .replace(/<style\b[^>]*\/\s*>/g, (match) => (
          match.includes(styleAttr) ? '' : match
        ));

      // If a <style> opener remains (multi-line body starts here), strip from
      // the opener to end-of-line and flip into skip mode.
      const opener = [...line.matchAll(/<style\b[^>]*>/g)]
        .find((match) => match[0].includes(styleAttr));
      if (opener) {
        line = line.slice(0, opener.index);
        inStyle = true;
      }
      out.push(line);
    } else {
      // In multi-line style body; drop everything until we see </style>.
      const closeIdx = line.search(/<\/style\s*>/);
      if (closeIdx !== -1) {
        inStyle = false;
        out.push(line.slice(closeIdx).replace(/<\/style\s*>/, ''));
      }
      // else: skip line entirely
    }
  }
  return out.join('\n');
}

/**
 * Find the inner content of `<TAG ...attrMatch...>…</TAG>` inside `text`,
 * handling nested same-tag elements via depth counting. `attrMatch` is a
 * literal attribute fragment that must appear inside the opener tag.
 * Returns the inner string (may be empty), or null if not found.
 */
function extractInnerByAttr(text, attrMatch) {
  const tags = scanJsxTags(text);
  const openerIndex = tags.findIndex((tag) => !tag.closing
    && !tag.selfClosing
    && tag.raw.includes(attrMatch));
  if (openerIndex === -1) return null;
  const opener = tags[openerIndex];
  const closing = findMatchingJsxTag(tags, openerIndex);
  return text.slice(opener.end, closing.start);
}

/**
 * Extract the original element content from within the variant wrapper.
 * Returns an array of lines.
 */
function extractOriginal(lines, block, id) {
  const text = stripStyleAndJoin(lines, block, id);
  const inner = extractInnerByAttr(text, 'data-impeccable-variant="original"');
  if (inner === null) return null;
  const result = inner.split('\n');
  while (result.length > 1 && result[0].trim() === '') result.shift();
  while (result.length > 1 && result[result.length - 1].trim() === '') result.pop();
  return result;
}

/**
 * Extract a specific variant's inner content (stripping the wrapper div).
 * Returns an array of lines, or null if not found.
 */
function extractVariant(lines, block, variantNum, id) {
  const text = stripStyleAndJoin(lines, block, id);
  const inner = extractInnerByAttr(text, 'data-impeccable-variant="' + variantNum + '"');
  if (inner === null) return null;
  const result = inner.split('\n');
  // Collapse a lone empty leading/trailing line (common after string splice).
  while (result.length > 1 && result[0].trim() === '') result.shift();
  while (result.length > 1 && result[result.length - 1].trim() === '') result.pop();
  return result.length > 0 ? result : null;
}

/**
 * Extract the colocated <style> block content (between the style tags).
 * Returns an array of CSS lines, or null if no style block found.
 *
 * Handles three shapes of `<style data-impeccable-css="ID" ...>`:
 *   1. Self-closing: `<style ... />` — no body; return null (nothing to carbonize).
 *   2. Same-line open+close: `<style>...</style>` — return the inner content.
 *   3. Multi-line: `<style>` on one line, `</style>` on a later line — return
 *      the lines between them.
 */
function extractCss(lines, block, id) {
  const styleAttr = 'data-impeccable-css="' + id + '"';
  let inStyle = false;
  const content = [];

  for (let i = block.start; i <= block.end; i++) {
    const line = lines[i];

    if (!inStyle && line.includes(styleAttr)) {
      // Self-closing: nothing to carbonize.
      if (/<style\b[^>]*\/\s*>/.test(line)) return null;
      // Same-line open + close: extract inner text.
      const sameLine = line.match(/<style\b[^>]*>([\s\S]*?)<\/style\s*>/);
      if (sameLine) {
        const inner = stripJsxTemplateWrap(sameLine[1]);
        return inner.length > 0 ? inner.split('\n') : null;
      }
      inStyle = true;
      continue; // skip the <style> opening tag
    }

    if (inStyle) {
      // Detect </style> anywhere on the line — JSX template-literal closes
      // (`}</style>`) put the close mid-line, and we don't want to absorb the
      // template-literal punctuation as CSS content.
      const closeIdx = line.indexOf('</style>');
      if (closeIdx !== -1) break;
      content.push(line);
    }
  }

  if (content.length === 0) return null;
  return stripJsxTemplateLines(content);
}

/**
 * Strip a JSX template-literal wrap (`{` … `}`) from CSS extracted out of a
 * `<style>` element in a JSX/TSX file. The agent may write the wrap with
 * `{` and `}` directly attached to the `<style>` tags, on their own lines,
 * or attached to the first/last CSS lines — all three are JSX-legal.
 *
 * Stripping is required because handleAccept re-wraps the CSS itself when
 * carbonizing. Without this, two consecutive accepts (or a previously-
 * accepted variants block being carbonized) would produce nested
 * `{` `{` … `}` `}`, which oxc rejects with "Expected `}` but found `@`".
 */
function stripJsxTemplateLines(content) {
  const out = content.slice();

  // Drop any leading blank lines so we don't miss a `{` line buried below
  // them; same for trailing.
  while (out.length > 0 && out[0].trim() === '') out.shift();
  while (out.length > 0 && out[out.length - 1].trim() === '') out.pop();
  if (out.length === 0) return null;

  // Leading `{`: own line, or attached to the first CSS line.
  const firstTrim = out[0].trimStart();
  if (firstTrim === '{`') {
    out.shift();
  } else if (firstTrim.startsWith('{`')) {
    const idx = out[0].indexOf('{`');
    out[0] = out[0].slice(0, idx) + out[0].slice(idx + 2);
    if (out[0].trim() === '') out.shift();
  }
  if (out.length === 0) return null;

  // Trailing `` ` `` `}`: own line, or attached to the last CSS line.
  const lastIdx = out.length - 1;
  const lastTrim = out[lastIdx].trimEnd();
  if (lastTrim === '`}') {
    out.pop();
  } else if (lastTrim.endsWith('`}')) {
    const text = out[lastIdx];
    const idx = text.lastIndexOf('`}');
    out[lastIdx] = text.slice(0, idx) + text.slice(idx + 2);
    if (out[lastIdx].trim() === '') out.pop();
  }

  return out.length > 0 ? out : null;
}

function stripJsxTemplateWrap(text) {
  const lines = text.split('\n');
  const stripped = stripJsxTemplateLines(lines);
  return stripped ? stripped.join('\n') : '';
}

/**
 * De-indent content that was indented by live-wrap.mjs.
 * The wrap script adds `indent + '    '` (4 extra spaces) to each line.
 * We restore to just `indent` level.
 */
function deindentContent(contentLines, baseIndent) {
  // Find the minimum indentation in the content to determine how much was added
  let minIndent = Infinity;
  for (const line of contentLines) {
    if (line.trim() === '') continue;
    const leadingSpaces = line.match(/^(\s*)/)[1].length;
    minIndent = Math.min(minIndent, leadingSpaces);
  }
  if (minIndent === Infinity) minIndent = 0;

  // Strip the extra indentation and re-add base indent
  return contentLines.map(line => {
    if (line.trim() === '') return '';
    return baseIndent + line.slice(minIndent);
  });
}

function detectCommentSyntax(filePath) {
  const ext = path.extname(filePath).toLowerCase();
  if (ext === '.jsx' || ext === '.tsx') {
    return { open: '{/*', close: '*/}' };
  }
  return { open: '<!--', close: '-->' };
}

// ---------------------------------------------------------------------------
// File search (find the file containing session markers)
// ---------------------------------------------------------------------------

function findSessionFile(id, cwd) {
  const searchDirs = ['src', 'app', 'pages', 'components', 'public', 'views', 'templates', '.'];
  const seen = new Set();
  const matches = [];

  for (const dir of searchDirs) {
    const absDir = path.join(cwd, dir);
    if (!fs.existsSync(absDir)) continue;
    searchDir(absDir, id, seen, 0, matches);
  }
  if (matches.length > 1) {
    const error = new Error(`Session ${id} has markers in multiple files`);
    error.code = 'session_markers_ambiguous';
    throw error;
  }
  if (matches.length === 0) return null;
  const snapshot = readContainedSource(cwd, matches[0]);
  const content = snapshot.bytes.toString('utf-8');
  return { file: snapshot.path, content, lines: content.split('\n'), snapshot };
}

function hasExecutableStartMarker(content, filePath, id) {
  const syntax = detectCommentSyntax(filePath);
  const marker = markerText('start', id, syntax);
  let offset = 0;
  for (const line of content.split('\n')) {
    const markerColumn = line.indexOf(marker);
    if (markerColumn === -1) {
      offset += line.length + 1;
      continue;
    }
    const markerOffset = offset + markerColumn;
    if (isExecutableMarker(content, markerOffset, marker.length, filePath, syntax)) {
      return true;
    }
    offset += line.length + 1;
  }
  return false;
}

function searchDir(dir, id, seen, depth, matches) {
  if (depth > 5) return;
  let realDir;
  try { realDir = fs.realpathSync(dir); } catch { return; }
  if (seen.has(realDir)) return;
  seen.add(realDir);

  let entries;
  try { entries = fs.readdirSync(dir, { withFileTypes: true }); }
  catch { return; }

  for (const entry of entries) {
    if (!entry.isFile()) continue;
    if (!EXTENSIONS.includes(path.extname(entry.name).toLowerCase())) continue;
    const filePath = path.join(dir, entry.name);
    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      if (hasExecutableStartMarker(content, filePath, id)) matches.push(filePath);
    } catch { /* skip */ }
  }

  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    if (['node_modules', '.git', 'dist', 'build'].includes(entry.name)) continue;
    searchDir(path.join(dir, entry.name), id, seen, depth + 1, matches);
  }
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

function argVal(args, flag) {
  const idx = args.indexOf(flag);
  return idx !== -1 && idx + 1 < args.length ? args[idx + 1] : null;
}

// Auto-execute when run directly
const _running = process.argv[1];
if (_running?.endsWith('live-accept.mjs') || _running?.endsWith('live-accept.mjs/')) {
  acceptCli().catch((error) => {
    console.error(JSON.stringify({
      handled: false,
      error: error.code || 'accept_failed',
      message: error.message,
      ...(error.rollbackErrors ? { rollbackErrors: error.rollbackErrors } : {}),
    }));
    process.exit(1);
  });
}

export { findMarkerBlock, extractOriginal, extractVariant, extractCss, deindentContent, detectCommentSyntax };
