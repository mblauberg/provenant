// Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md.
/**
 * CLI helper: find an element in source and wrap it in a variant container.
 *
 * Usage:
 *   node live-wrap.mjs --id SESSION_ID --count N --query "hero-combined-left" [--file path]
 *
 * Searches project files for the element matching the query (class name, ID, or
 * text snippet), wraps it with the variant scaffolding, and prints the file path
 * + line range where the agent should insert variant HTML.
 *
 * This replaces 3-4 agent tool calls (grep + read + edit) with a single CLI call.
 */

import fs from 'node:fs';
import path from 'node:path';
import { isGeneratedFile } from './is-generated.mjs';
import {
  readContainedSource,
  replaceContainedSource,
  resolveContainedSourcePath,
} from './contained-source.mjs';
import {
  findJsxSubtree,
  frameworkTemplateContextAtOffset,
  hasExecutableJsxTagAtOffset,
  htmlLexicalContextAtOffset,
  isOffsetInsideAstroFrontmatter,
  scanJsxTagAtOffset,
} from './jsx-tag-scanner.mjs';

const EXTENSIONS = ['.html', '.jsx', '.tsx', '.vue', '.svelte', '.astro'];
const SESSION_ID_PATTERN = /^[A-Za-z0-9_-]{1,128}$/;

function resolveProjectSourceFile(projectRoot, requestedPath) {
  return resolveContainedSourcePath(projectRoot, requestedPath, { relativeOnly: true });
}

export async function wrapCli() {
  const args = process.argv.slice(2);

  if (args.includes('--help') || args.includes('-h')) {
    console.log(`Usage: node live-wrap.mjs [options]

Find an element in source and wrap it in a variant container.

Required:
  --id ID            Session ID for the variant wrapper
  --count N          Number of expected variants (1-8)

Element identification (at least one required):
  --element-id ID    HTML id attribute of the element
  --classes A,B,C    Comma-separated CSS class names
  --tag TAG          Tag name (div, section, etc.)
  --query TEXT       Fallback: raw text to search for

Optional:
  --file PATH        Existing project-relative regular file (skips auto-detection;
                     symlinks and multiply-linked files are rejected)
  --text TEXT        Picked element's textContent. Used to disambiguate when
                     classes/tag match multiple sibling elements (e.g. a list
                     of <Card>s with the same className). Pass the first ~80
                     chars of event.element.textContent.
  --help             Show this help message

Output (JSON):
  { file, startLine, endLine, insertLine, commentSyntax }

The agent should insert variant HTML at insertLine.`);
    process.exit(0);
  }

  const id = argVal(args, '--id');
  const countRaw = argVal(args, '--count') || '3';
  const count = Number(countRaw);
  const elementId = argVal(args, '--element-id');
  const classes = argVal(args, '--classes');
  const tag = argVal(args, '--tag');
  const query = argVal(args, '--query');
  const filePath = argVal(args, '--file');
  const text = argVal(args, '--text');

  if (!id) { console.error('Missing --id'); process.exit(1); }
  if (!SESSION_ID_PATTERN.test(id)) {
    console.error(JSON.stringify({ error: 'invalid_session_id' }));
    process.exit(1);
  }
  if (!/^[1-8]$/.test(countRaw) || !Number.isInteger(count)) {
    console.error(JSON.stringify({ error: 'invalid_variant_count' }));
    process.exit(1);
  }
  if (!elementId && !classes && !query) {
    console.error('Need at least one of: --element-id, --classes, --query');
    process.exit(1);
  }

  // Build search queries in priority order (most specific first)
  const queries = buildSearchQueries(elementId, classes, tag, query);

  const genOpts = { cwd: process.cwd() };

  // Find the source file. Generated files are excluded from auto-search so we
  // don't silently write variants into a file the next build will wipe.
  let targetFile = filePath;
  let matchedQuery = null;
  if (!targetFile) {
    const candidates = findProjectCandidates(queries, text, process.cwd(), genOpts, tag);
    if (candidates.length > 1) {
        console.error(JSON.stringify({
          error: 'element_ambiguous',
          fallback: 'agent-driven',
          candidates: candidates.map((candidate) => ({
            file: path.relative(process.cwd(), candidate.file),
            startLine: candidate.startLine + 1,
            endLine: candidate.endLine + 1,
          })),
          hint: 'Multiple source elements match this identity. Pass --file, --element-id, or --text, or fall back to agent-driven wrapping.',
        }));
        process.exit(1);
    }
    if (candidates.length === 1) {
      targetFile = candidates[0].file;
      matchedQuery = candidates[0].query;
    }
    if (!targetFile) {
      // Nothing in source. Did the element show up in a generated file? That
      // tells the agent "fall back to the agent-driven flow" vs "element just
      // doesn't exist in this project."
      let generatedHit = null;
      for (const q of queries) {
        generatedHit = findFileWithQuery(q, process.cwd(), { ...genOpts, includeGenerated: true });
        if (generatedHit) break;
      }
      if (generatedHit) {
        console.error(JSON.stringify({
          error: 'element_not_in_source',
          fallback: 'agent-driven',
          generatedMatch: path.relative(process.cwd(), generatedHit),
          hint: 'Element found only in a generated file. See "Handle fallback" in live.md.',
        }));
      } else {
        console.error(JSON.stringify({
          error: 'element_not_found',
          fallback: 'agent-driven',
          hint: 'Element not found in any project file. It may be runtime-injected (JS component, etc.). See "Handle fallback" in live.md.',
        }));
      }
      process.exit(1);
    }
  } else {
    matchedQuery = queries[0];
  }

  let sourceSnapshot;
  try {
    sourceSnapshot = readContainedSource(process.cwd(), targetFile, { relativeOnly: !!filePath });
    targetFile = sourceSnapshot.path;
  } catch (error) {
    console.error(JSON.stringify({
      error: error.code || 'source_path_invalid',
      fallback: 'agent-driven',
      hint: error.message,
    }));
    process.exit(1);
  }
  if (isGeneratedFile(targetFile, genOpts)) {
    console.error(JSON.stringify({
      error: 'file_is_generated',
      fallback: 'agent-driven',
      file: path.relative(process.cwd(), targetFile),
      hint: 'The target is generated. Writing here gets wiped by the next build. See "Handle fallback" in live.md.',
    }));
    process.exit(1);
  }

  const content = sourceSnapshot.bytes.toString('utf-8');
  const lines = content.split('\n');
  const newline = content.includes('\r\n') ? '\r\n' : '\n';
  const commentSyntax = detectCommentSyntax(targetFile);
  const isJsx = commentSyntax.open === '{/*';

  // Find the element, trying each query in priority order. When `--text` is
  // supplied, collect every candidate the queries surface and require the
  // picked element's textContent to prove one target. Without `--text`, keep
  // the legacy first-match behavior for callers that did not provide the
  // stronger browser identity signal.
  let match = null;
  let selectionError = null;
  for (const q of queries) {
    let candidates = [];
    try { candidates = findAllElements(lines, q, tag, isJsx, targetFile); }
    catch (error) {
      selectionError ??= error;
      continue;
    }
    if (candidates.length > 1 && text) {
      candidates = filterByText(candidates, content, text);
    }
    if (candidates.length === 1) {
      match = candidates[0];
      break;
    }
    if (candidates.length > 1) {
      console.error(JSON.stringify({
        error: 'element_ambiguous',
        fallback: 'agent-driven',
        file: path.relative(process.cwd(), targetFile),
        candidates: candidates.map((c) => ({
          startLine: c.startLine + 1,
          endLine: c.endLine + 1,
        })),
        hint: 'Multiple source elements match both classes/tag and textContent. Pass --element-id, a more specific --text, or use agent-driven wrapping.',
      }));
      process.exit(1);
    }
  }
  if (!match) {
    if (selectionError) throw selectionError;
    console.error(JSON.stringify({ error: 'Found file but could not locate element in ' + targetFile + '. Searched for: ' + queries.join(', ') }));
    process.exit(1);
  }

  const { startLine, endLine, startOffset, endOffset } = match;
  const styleMode = detectStyleMode(targetFile);
  const indent = lines[startLine].match(/^(\s*)/)[1];

  // Extract the original element. Reindent under the wrapper while preserving
  // the relative depth between lines — `l.trimStart()` would strip ALL leading
  // whitespace and collapse e.g. `<aside>`/`  <h1>`/`</aside>` (6/8/6 spaces)
  // to a single uniform indent, so on accept/discard the round-trip restores
  // the inner element at its parent's depth instead of nested inside it.
  // Strip only the COMMON minimum leading whitespace across the picked lines;
  // `deindentContent` on the accept side already mirrors this convention.
  const originalSource = content.slice(startOffset, endOffset);
  const originalLines = originalSource.split(/\r?\n/);
  const originalBaseIndent = minLeadingSpaces(originalLines);
  const reindentOriginal = (extra) => originalLines
    .map((l) => (l.trim() === '' ? '' : indent + extra + l.slice(originalBaseIndent)))
    .join('\n');
  const originalIndented = reindentOriginal('    ');

  // Wrapper attributes differ by syntax. HTML allows plain string attrs;
  // JSX requires object-literal style and parses string attrs as HTML (which
  // either type-errors or renders a literal CSS string).
  const styleContents = isJsx ? 'style={{ display: "contents" }}' : 'style="display: contents"';

  // JSX/TSX guard: the picked element occupies a single JSX child slot
  // (inside `return (...)`, an array `.map(...)`, an `asChild` branch, or
  // any other expression position). Replacing it with `comment + <div> +
  // comment` yields three adjacent siblings — invalid JSX. We can't use a
  // Fragment `<></>` either: parents that clone children (Radix `asChild`,
  // Headless UI, etc.) hit "Invalid prop supplied to React.Fragment" when
  // they try to pass an `id` through.
  //
  // Solution: keep the wrapper `<div>` as the single JSX-slot child and
  // tuck both marker comments INSIDE it. accept/discard then expands its
  // replacement range to include the wrapper's `<div>` open / close lines
  // so the entire scaffold gets removed cleanly.
  const wrapperLines = isJsx ? [
    indent + '<div data-impeccable-variants="' + id + '" data-impeccable-variant-count="' + count + '" ' + styleContents + '>',
    indent + '  ' + commentSyntax.open + ' impeccable-variants-start ' + id + ' ' + commentSyntax.close,
    indent + '  ' + commentSyntax.open + ' Original ' + commentSyntax.close,
    indent + '  <div data-impeccable-variant="original">',
    reindentOriginal('    '),
    indent + '  </div>',
    indent + '  ' + commentSyntax.open + ' Variants: insert below this line ' + commentSyntax.close,
    indent + '  ' + commentSyntax.open + ' impeccable-variants-end ' + id + ' ' + commentSyntax.close,
    indent + '</div>',
  ] : [
    indent + commentSyntax.open + ' impeccable-variants-start ' + id + ' ' + commentSyntax.close,
    indent + '<div data-impeccable-variants="' + id + '" data-impeccable-variant-count="' + count + '" ' + styleContents + '>',
    indent + '  ' + commentSyntax.open + ' Original ' + commentSyntax.close,
    indent + '  <div data-impeccable-variant="original">',
    originalIndented,
    indent + '  </div>',
    indent + '  ' + commentSyntax.open + ' Variants: insert below this line ' + commentSyntax.close,
    indent + '</div>',
    indent + commentSyntax.open + ' impeccable-variants-end ' + id + ' ' + commentSyntax.close,
  ];

  // Replace the original element with the wrapper
  const wrappedSource = wrapperLines.join(newline);
  const replacementStartOffset = includeLeadingLineIndent(content, startOffset);
  const updatedSource = content.slice(0, replacementStartOffset)
    + wrappedSource
    + content.slice(endOffset);
  try {
    replaceContainedSource(sourceSnapshot, updatedSource);
  } catch (error) {
    console.error(JSON.stringify({
      error: error.code || 'source_replace_failed',
      fallback: 'agent-driven',
      hint: error.message,
    }));
    process.exit(1);
  }

  // Calculate insert line (the "insert below this line" comment).
  // 0-indexed file position. Both HTML and JSX wrappers have 6 lines above
  // the insert marker (HTML: start-comment + outer-div + Original-comment +
  // original-div + content + close-original-div; JSX: outer-div +
  // start-comment + Original-comment + original-div + content +
  // close-original-div). Multi-line originals push the marker by their
  // extra line count.
  const insertLine = startLine + 6 + (originalLines.length - 1);

  console.log(JSON.stringify({
    file: path.relative(process.cwd(), targetFile),
    startLine: startLine + 1,       // 1-indexed for the agent
    // wrapperLines is an array but one element (the original-content slot)
    // is a `\n`-joined multi-line string, so the actual file-row count is
    // wrapperLines.length + (originalLines.length - 1). Without the offset,
    // endLine pointed inside the wrapper for any picked element that
    // spanned more than one source line.
    endLine: startLine + wrapperLines.length + (originalLines.length - 1), // 1-indexed
    insertLine: insertLine + 1,     // 1-indexed: where variants go
    commentSyntax: commentSyntax,
    styleMode: styleMode.mode,
    styleTag: styleMode.styleTag,
    cssSelectorPrefixExamples: buildCssSelectorPrefixExamples(styleMode.mode, count),
    cssAuthoring: buildCssAuthoring(styleMode, count),
    originalLineCount: originalLines.length,
  }));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function argVal(args, flag) {
  const idx = args.indexOf(flag);
  return idx !== -1 && idx + 1 < args.length ? args[idx + 1] : null;
}

/**
 * Build search query strings in priority order (most specific first).
 * ID is most reliable, then specific class combos, then single classes, then raw query.
 */
function buildSearchQueries(elementId, classes, tag, query) {
  const queries = [];

  // 1. ID is the most specific
  if (elementId) {
    queries.push('id="' + elementId + '"');
  }

  // 2. Full class attribute match (for elements with distinctive multi-class combos).
  // Emit both class="..." (HTML) and className="..." (React/JSX) so whichever
  // convention the file uses will match.
  if (classes) {
    const classList = classes.split(',').map(c => c.trim()).filter(Boolean);
    if (classList.length > 1) {
      const joined = classList.join(' ');
      const sorted = [...classList].sort((a, b) => b.length - a.length);
      queries.push('class="' + joined + '"');
      queries.push('className="' + joined + '"');
      queries.push(sorted[0]); // most distinctive single class, fallback
    } else if (classList.length === 1) {
      queries.push(classList[0]);
    }
  }

  // 3. Tag + class combo (e.g., <section class="hero">).
  // Same dual-emit for JSX compatibility.
  if (tag && classes) {
    const firstClass = classes.split(',')[0].trim();
    queries.push('<' + tag + ' class="' + firstClass);
    queries.push('<' + tag + ' className="' + firstClass);
  }

  // 4. Raw fallback query
  if (query) {
    queries.push(query);
  }

  return queries;
}

function detectCommentSyntax(filePath) {
  const ext = path.extname(filePath).toLowerCase();
  if (ext === '.jsx' || ext === '.tsx') {
    return { open: '{/*', close: '*/}' };
  }
  // HTML, Vue, Svelte, Astro all use HTML comments
  return { open: '<!--', close: '-->' };
}

function detectStyleMode(filePath) {
  const ext = path.extname(filePath).toLowerCase();
  if (ext === '.astro') {
    return {
      mode: 'astro-global-prefixed',
      styleTag: '<style is:inline data-impeccable-css="SESSION_ID">',
    };
  }
  return {
    mode: 'scoped',
    styleTag: '<style data-impeccable-css="SESSION_ID">',
  };
}

function buildCssSelectorPrefixExamples(styleMode, count) {
  if (styleMode !== 'astro-global-prefixed') return [];
  return Array.from({ length: count }, (_, i) => `[data-impeccable-variant="${i + 1}"]`);
}

function buildCssAuthoring(styleMode, count) {
  const variantNumbers = Array.from({ length: count }, (_, i) => i + 1);
  if (styleMode.mode === 'astro-global-prefixed') {
    return {
      mode: styleMode.mode,
      styleTag: styleMode.styleTag,
      strategy: 'global-prefixed',
      rulePattern: '[data-impeccable-variant="N"] > .variant-class { ... }',
      selectorExamples: variantNumbers.map((n) => `[data-impeccable-variant="${n}"] > .variant-class`),
      requirements: [
        'Use the styleTag exactly; the is:inline attribute is required for this file.',
        'Prefix every preview selector with the matching [data-impeccable-variant="N"] selector.',
        'Keep selectors anchored to the generated variant wrapper; do not rely on component CSS scoping for preview rules.',
      ],
      forbidden: [
        'Do not use @scope for this styleMode.',
      ],
    };
  }
  return {
    mode: styleMode.mode,
    styleTag: styleMode.styleTag,
    strategy: 'scope-rule',
    rulePattern: '@scope ([data-impeccable-variant="N"]) { :scope > .variant-class { ... } }',
    selectorExamples: variantNumbers.map((n) => `@scope ([data-impeccable-variant="${n}"]) { :scope > .variant-class { ... } }`),
    requirements: [
      'Use @scope blocks keyed to each [data-impeccable-variant="N"] wrapper.',
      'Inside each @scope block, make :scope rules step into the replacement element with a descendant combinator.',
      'Use the styleTag exactly; do not add framework-specific style attributes unless this object says to.',
    ],
    forbidden: [
      'Do not use global [data-impeccable-variant="N"] selector prefixes for this styleMode.',
      'Do not add is:inline to the style tag for this styleMode.',
    ],
  };
}

/**
 * Search project files for the query string (class name, ID, etc.)
 * Returns the first matching file path, or null.
 */
function findFileWithQuery(query, cwd, genOpts = {}) {
  const searchDirs = ['src', 'app', 'pages', 'components', 'public', 'views', 'templates', '.'];
  const seen = new Set();

  for (const dir of searchDirs) {
    const absDir = path.join(cwd, dir);
    if (!fs.existsSync(absDir)) continue;
    const result = searchDir(absDir, query, seen, 0, genOpts);
    if (result) return result;
  }
  return null;
}

function findFilesWithQuery(query, cwd, genOpts = {}) {
  const searchDirs = ['src', 'app', 'pages', 'components', 'public', 'views', 'templates', '.'];
  const seenDirs = new Set();
  const seenFiles = new Set();
  const matches = [];
  for (const dir of searchDirs) {
    const absDir = path.join(cwd, dir);
    if (!fs.existsSync(absDir)) continue;
    searchDirForAll(absDir, query, seenDirs, seenFiles, matches, 0, genOpts);
  }
  return matches;
}

function searchDirForAll(dir, query, seenDirs, seenFiles, matches, depth, genOpts) {
  if (depth > 5) return;
  const realDir = fs.realpathSync(dir);
  if (seenDirs.has(realDir)) return;
  seenDirs.add(realDir);
  let entries;
  try { entries = fs.readdirSync(dir, { withFileTypes: true }).sort((a, b) => a.name.localeCompare(b.name)); }
  catch { return; }
  for (const entry of entries) {
    if (!entry.isFile()) continue;
    const ext = path.extname(entry.name).toLowerCase();
    if (!EXTENSIONS.includes(ext)) continue;
    const filePath = path.join(dir, entry.name);
    const realFile = fs.realpathSync(filePath);
    if (seenFiles.has(realFile)) continue;
    seenFiles.add(realFile);
    if (!genOpts.includeGenerated && isGeneratedFile(filePath, genOpts)) continue;
    try {
      if (fs.readFileSync(filePath, 'utf-8').includes(query)) matches.push(filePath);
    } catch { /* skip unreadable files */ }
  }
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    if (entry.name === 'node_modules' || entry.name === '.git') continue;
    searchDirForAll(path.join(dir, entry.name), query, seenDirs, seenFiles, matches, depth + 1, genOpts);
  }
}

function findProjectCandidates(queries, text, cwd, genOpts, tag) {
  for (const query of queries) {
    const candidates = [];
    const seen = new Set();
    for (const file of findFilesWithQuery(query, cwd, genOpts)) {
      let source;
      try { source = fs.readFileSync(file, 'utf-8'); }
      catch { continue; }
      const lines = source.split('\n');
      const isJsx = detectCommentSyntax(file).open === '{/*';
      let found = [];
      try { found = findAllElements(lines, query, tag, isJsx, file); }
      catch { continue; }
      for (const candidate of found) {
        const key = `${fs.realpathSync(file)}:${candidate.startOffset}:${candidate.endOffset}`;
        if (seen.has(key)) continue;
        seen.add(key);
        candidates.push({ ...candidate, file, query, source });
      }
    }
    if (candidates.length === 1) return candidates;
    if (candidates.length > 1) {
      if (!text) return candidates;
      const filtered = candidates.filter(
        (candidate) => filterByText([candidate], candidate.source, text).length === 1,
      );
      if (filtered.length > 0) return filtered;
    }
  }
  return [];
}

function searchDir(dir, query, seen, depth, genOpts) {
  if (depth > 5) return null; // don't go too deep
  const realDir = fs.realpathSync(dir);
  if (seen.has(realDir)) return null;
  seen.add(realDir);

  let entries;
  try { entries = fs.readdirSync(dir, { withFileTypes: true }); }
  catch { return null; }

  // Check files first
  for (const entry of entries) {
    if (!entry.isFile()) continue;
    const ext = path.extname(entry.name).toLowerCase();
    if (!EXTENSIONS.includes(ext)) continue;

    const filePath = path.join(dir, entry.name);
    if (!genOpts.includeGenerated && isGeneratedFile(filePath, genOpts)) continue;
    try {
      const content = fs.readFileSync(filePath, 'utf-8');
      if (content.includes(query)) return filePath;
    } catch { /* skip unreadable files */ }
  }

  // Then recurse into directories. Always skip node_modules and .git (never
  // project content). dist/build/out are left to the isGeneratedFile guard so
  // the includeGenerated second-pass can still find the element there and
  // report `generatedMatch`.
  for (const entry of entries) {
    if (!entry.isDirectory()) continue;
    if (entry.name === 'node_modules' || entry.name === '.git') continue;
    const result = searchDir(path.join(dir, entry.name), query, seen, depth + 1, genOpts);
    if (result) return result;
  }

  return null;
}

/**
 * Regex that matches a tag opener on a line. Allows the tag name to be
 * followed by whitespace, `>`, `/`, or end-of-line so that multi-line JSX
 * openers (e.g. `<section\n  className="..."\n>`) are recognised.
 */
const OPENER_RE = /<([A-Za-z][A-Za-z0-9]*)(?=[\s/>]|$)/;

/**
 * Find the element's start and end line in the file.
 *
 * `query` is a class name, attribute fragment (`class="..."`, `className="..."`,
 * `id="..."`), or a raw text snippet. Because a query can appear on a
 * continuation line of a multi-line tag (e.g. the `className="..."` row of a
 * `<section\n  className="..."\n>` JSX tag), we walk backward from the match
 * line to find the actual tag opener. When `tag` is provided, opener candidates
 * must match that tag name.
 */
/**
 * Return the smallest leading-whitespace count across a set of lines,
 * ignoring blank lines (whose indent isn't load-bearing). Used to compute
 * the common base indent of a multi-line picked element so reindenting
 * under the wrapper preserves the relative depth between lines.
 */
function minLeadingSpaces(lines) {
  let min = Infinity;
  for (const l of lines) {
    if (l.trim() === '') continue;
    const m = l.match(/^(\s*)/);
    if (m && m[1].length < min) min = m[1].length;
  }
  return min === Infinity ? 0 : min;
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

function isExecutableOpener(
  lines,
  openerLine,
  source,
  offsets,
  isJsx,
  filePath,
  openerIndex = null,
) {
  const opener = openerIndex === null
    ? lines[openerLine].match(OPENER_RE)
    : lines[openerLine].slice(openerIndex).match(OPENER_RE);
  if (!opener) return false;
  const offset = offsets[openerLine] + (openerIndex ?? opener.index);
  if (isJsx) return hasExecutableJsxTagAtOffset(source, offset);
  if (htmlLexicalContextAtOffset(source, offset) !== 'markup') return false;
  const extension = path.extname(filePath || '').toLowerCase();
  if (!['.astro', '.svelte', '.vue'].includes(extension)) return true;
  if (extension === '.astro' && isOffsetInsideAstroFrontmatter(source, offset)) return false;
  try {
    return frameworkTemplateContextAtOffset(source, offset) === 'markup';
  } catch {
    return false;
  }
}

function findElement(lines, query, tag = null, isJsx = false, filePath = '') {
  const source = lines.join('\n');
  const offsets = buildLineOffsets(lines);
  // Iterate all matches — the first substring hit isn't always the right one.
  for (let i = 0; i < lines.length; i++) {
    if (!lines[i].includes(query)) continue;

    const stripped = lines[i].trim();
    if (stripped.startsWith('<!--') || stripped.startsWith('{/*') || stripped.startsWith('//')) continue;
    // Skip lines already inside a variant wrapper
    if (lines[i].includes('data-impeccable-variant')) continue;
    const opener = findOpener(lines, i, query, tag);
    if (!opener) continue;
    if (!isExecutableOpener(lines, opener.line, source, offsets, isJsx, filePath, opener.index)) continue;

    return findElementRange(source, lines, offsets, opener.line, { isJsx, openerIndex: opener.index });
  }

  return null;
}

/**
 * Like findElement, but returns every match. Used for ambiguity detection
 * when the agent passes --text: when the same className appears on multiple
 * sibling elements (a list of cards, repeated section variants, etc.),
 * first-match silently lands on the wrong branch. Returning all matches lets
 * the caller narrow by textContent or fail with a structured ambiguity error.
 */
function findAllElements(lines, query, tag = null, isJsx = false, filePath = '') {
  const out = [];
  const seen = new Set();
  const source = lines.join('\n');
  const offsets = buildLineOffsets(lines);
  for (let i = 0; i < lines.length; i++) {
    if (!lines[i].includes(query)) continue;
    const stripped = lines[i].trim();
    if (stripped.startsWith('<!--') || stripped.startsWith('{/*') || stripped.startsWith('//')) continue;
    if (lines[i].includes('data-impeccable-variant')) continue;
    for (const opener of findCandidateOpeners(lines, i, query, tag)) {
      if (!isExecutableOpener(lines, opener.line, source, offsets, isJsx, filePath, opener.index)) continue;
      const openerOffset = offsets[opener.line] + opener.index;
      if (seen.has(openerOffset)) continue;
      const range = findElementRange(source, lines, offsets, opener.line, {
        isJsx,
        openerIndex: opener.index,
      });
      if (!source.slice(range.startOffset, range.endOffset).includes(query)) continue;
      seen.add(openerOffset);
      out.push(range);
    }
  }
  return out;
}

function findCandidateOpeners(lines, matchLine, query, tag) {
  const line = lines[matchLine];
  const sameLine = [...line.matchAll(/<([A-Za-z][A-Za-z0-9]*)(?=[\s/>]|$)/g)]
    .filter((candidate) => !tag || candidate[1] === tag)
    .map((candidate) => ({ line: matchLine, index: candidate.index, name: candidate[1] }));
  const containing = sameLine.filter((candidate) => {
    try { return scanJsxTagAtOffset(line, candidate.index)?.raw.includes(query); }
    catch { return false; }
  });
  if (containing.length > 0) return containing;
  if (sameLine.some((candidate) => ['script', 'style'].includes(candidate.name.toLowerCase()))) {
    return [];
  }
  if (sameLine.length > 0) return sameLine;
  const fallback = findOpener(lines, matchLine, query, tag);
  return fallback ? [fallback] : [];
}

function findElementRange(source, lines, offsets, startLine, { isJsx, openerIndex = null }) {
  const openerMatch = openerIndex === null
    ? lines[startLine].match(OPENER_RE)
    : lines[startLine].slice(openerIndex).match(OPENER_RE);
  if (!openerMatch) throw new Error('Selected source line has no element opener');
  const startOffset = offsets[startLine] + (openerIndex ?? openerMatch.index);
  const opener = scanJsxTagAtOffset(source, startOffset);
  if (!opener) throw new Error('Selected source offset has no element opener');
  if (!isJsx && HTML_VOID_TAGS.has(opener.name.toLowerCase())) {
    return {
      startLine,
      endLine: startLine + source.slice(startOffset, opener.end).split('\n').length - 1,
      startOffset,
      endOffset: opener.end,
    };
  }
  let closing;
  try {
    ({ closing } = findJsxSubtree(
      source.slice(startOffset),
      (tag) => tag.start === 0 && tag.name === opener.name,
      { strictNesting: isJsx },
    ));
  } catch (error) {
    if (!isJsx
      && HTML_OPTIONAL_END_TAGS.has(opener.name.toLowerCase())
      && error.message.startsWith('Missing closing JSX tag')) {
      const fallback = new Error('Selected HTML elements with implicit end tags require manual wrapping');
      fallback.code = 'html_implicit_end_unsupported';
      throw fallback;
    }
    throw error;
  }
  const endOffset = startOffset + closing.end;
  return {
    startLine,
    endLine: startLine + source.slice(startOffset, endOffset).split('\n').length - 1,
    startOffset,
    endOffset,
  };
}

/**
 * Narrow a candidate set to those whose source body matches a meaningful
 * prefix of the picked element's textContent. The compare strips tags and
 * JSX expressions, then checks two whitespace normalizations side-by-side:
 *
 *   - single-space ("hero two second card body")
 *   - no-whitespace ("herotwosecondcardbody")
 *
 * Both are needed because `el.textContent` concatenates sibling text without
 * inserting whitespace (e.g. `<h1>Hero Two</h1><p>Second…</p>` reads as
 * `"Hero TwoSecond…"`), while the source has whitespace between tags. If
 * Either normalization matches, the candidate keeps. Short visible labels
 * remain useful when they identify exactly one structural candidate.
 */
function filterByText(candidates, source, text) {
  const trimmed = text.replace(/\s+/g, ' ').trim().toLowerCase().slice(0, 80);
  if (trimmed.length === 0) return [];
  const targetSpaced = trimmed;
  const targetCompact = trimmed.replace(/\s+/g, '');

  return candidates.filter((c) => {
    const body = source.slice(c.startOffset, c.endOffset);
    const inner = body
      .replace(/<[^>]*>/g, ' ')   // strip HTML/JSX tags
      .replace(/\{[^}]*\}/g, ' ')  // strip JSX expressions
      .toLowerCase();
    const sourceSpaced = inner.replace(/\s+/g, ' ').trim();
    const sourceCompact = inner.replace(/\s+/g, '');
    return sourceSpaced.includes(targetSpaced) || sourceCompact.includes(targetCompact);
  });
}

/**
 * Resolve a match line to the real tag opener. If the match line itself opens
 * a tag, return it. Otherwise walk up to 10 lines backward looking for the
 * first tag opener. If `tag` is specified, the opener must match that tag
 * name; an opener with a different tag name aborts the backward walk for this
 * match (we don't jump across element boundaries).
 *
 * Returns the line index of the opener, or -1 if none can be resolved.
 */
function findOpener(lines, matchLine, query, tag) {
  const line = lines[matchLine];
  const occurrences = [];
  for (let index = line.indexOf(query); index !== -1; index = line.indexOf(query, index + 1)) {
    occurrences.push(index);
  }
  const openers = [...line.matchAll(/<([A-Za-z][A-Za-z0-9]*)(?=[\s/>]|$)/g)]
    .filter((candidate) => !tag || candidate[1] === tag);
  if (occurrences.length > 1 && openers.length > 1) return null;
  if (openers.length > 0 && occurrences.length === 1) {
    const queryIndex = occurrences[0];
    const containing = openers.filter((candidate) => {
      try {
        return scanJsxTagAtOffset(line, candidate.index)?.raw.includes(query);
      } catch {
        return false;
      }
    });
    if (containing.length === 1) return { line: matchLine, index: containing[0].index };
    if (containing.length > 1) return null;
    const preceding = openers.filter((candidate) => candidate.index <= queryIndex);
    if (preceding.length > 0) {
      return { line: matchLine, index: preceding.at(-1).index };
    }
  }
  const MAX_BACKWALK = 10;
  for (let i = matchLine - 1; i >= Math.max(0, matchLine - MAX_BACKWALK); i--) {
    const opener = lines[i].match(OPENER_RE);
    if (!opener) continue;
    if (!tag || opener[1] === tag) return { line: i, index: opener.index };
    // Different tag name than requested — abort; we're inside a non-target opener.
    return null;
  }
  return null;
}

/**
 * Starting from a line with an opening tag, find the line with the matching
 * closing tag by counting tag nesting depth.
 */
const HTML_VOID_TAGS = new Set([
  'area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input',
  'link', 'meta', 'param', 'source', 'track', 'wbr',
]);

const HTML_OPTIONAL_END_TAGS = new Set([
  'dd', 'dt', 'li', 'optgroup', 'option', 'p', 'rp', 'rt',
  'tbody', 'td', 'tfoot', 'th', 'thead', 'tr',
]);

function findClosingLine(lines, start, { isJsx = true } = {}) {
  const openMatch = lines[start].match(OPENER_RE);
  if (!openMatch) return start; // caller passed a non-opener; nothing to span

  const tagName = openMatch[1];
  if (!isJsx && HTML_VOID_TAGS.has(tagName.toLowerCase())) return start;
  const joined = lines.slice(start).join('\n');
  let closing;
  try {
    ({ closing } = findJsxSubtree(
      joined,
      (tag) => tag.name === tagName,
      { strictNesting: isJsx },
    ));
  } catch (error) {
    if (!isJsx
      && HTML_OPTIONAL_END_TAGS.has(tagName.toLowerCase())
      && error.message.startsWith('Missing closing JSX tag')) {
      const fallback = new Error(
        'Selected HTML elements with implicit end tags require manual wrapping',
      );
      fallback.code = 'html_implicit_end_unsupported';
      throw fallback;
    }
    throw error;
  }
  return start + joined.slice(0, closing.end).split('\n').length - 1;
}

// Auto-execute when run directly (node live-wrap.mjs ...)
const _running = process.argv[1];
if (_running?.endsWith('live-wrap.mjs') || _running?.endsWith('live-wrap.mjs/')) {
  wrapCli().catch((error) => {
    console.error(JSON.stringify({
      error: error.code || 'wrap_failed',
      fallback: 'agent-driven',
      hint: error.message,
    }));
    process.exit(1);
  });
}

// Test exports (used by tests/live-wrap.test.mjs)
export {
  buildSearchQueries,
  findElement,
  findClosingLine,
  detectCommentSyntax,
  resolveProjectSourceFile,
};
