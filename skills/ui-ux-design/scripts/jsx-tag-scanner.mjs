function unbalanced(message) {
  const error = new Error(message);
  error.code = 'jsx_scan_unbalanced';
  return error;
}

function skipQuoted(source, start, quote) {
  let escaped = false;
  for (let index = start + 1; index < source.length; index += 1) {
    const char = source[index];
    if (escaped) {
      escaped = false;
    } else if (char === '\\') {
      escaped = true;
    } else if (char === quote) {
      return index + 1;
    }
  }
  throw unbalanced('Unterminated JSX string');
}

function skipBlockComment(source, start) {
  const end = source.indexOf('*/', start + 2);
  if (end === -1) throw unbalanced('Unterminated JSX block comment');
  return end + 2;
}

function skipLineComment(source, start) {
  const end = source.indexOf('\n', start + 2);
  return end === -1 ? source.length : end + 1;
}

function findMatchingOpenParen(source, closeIndex) {
  const stack = [];
  for (let index = 0; index <= closeIndex; index += 1) {
    const char = source[index];
    if (char === '"' || char === "'") {
      index = skipQuoted(source, index, char) - 1;
    } else if (char === '`') {
      index = skipTemplateLiteral(source, index) - 1;
    } else if (source.startsWith('/*', index)) {
      index = skipBlockComment(source, index) - 1;
    } else if (source.startsWith('//', index)) {
      index = skipLineComment(source, index) - 1;
    } else if (char === '/' && canStartRegexLiteral(source, index)) {
      index = skipRegexLiteral(source, index) - 1;
    } else if (char === '(') {
      stack.push(index);
    } else if (char === ')') {
      const open = stack.pop();
      if (index === closeIndex) return open ?? -1;
    }
  }
  return -1;
}

function followsControlHeader(source, closeIndex) {
  if (source[closeIndex] !== ')') return false;
  const openIndex = findMatchingOpenParen(source, closeIndex);
  if (openIndex < 0) return false;
  const prefix = source.slice(0, openIndex).match(/([A-Za-z_$][A-Za-z0-9_$]*)\s*$/);
  return ['for', 'if', 'while', 'with'].includes(prefix?.[1]);
}

function canStartRegexLiteral(source, start) {
  let before = start - 1;
  while (before >= 0 && /\s/.test(source[before])) before -= 1;
  if (before < 0 || /[=(:,\[{!&|?;+*%^~<>]/.test(source[before])) return true;
  return followsControlHeader(source, before)
    || /\b(?:await|case|delete|do|else|in|instanceof|return|throw|typeof|void|yield)\s*$/.test(
      source.slice(0, start),
    );
}

function skipRegexLiteral(source, start) {
  let escaped = false;
  let inClass = false;
  for (let index = start + 1; index < source.length; index += 1) {
    const char = source[index];
    if (escaped) {
      escaped = false;
    } else if (char === '\\') {
      escaped = true;
    } else if (char === '[') {
      inClass = true;
    } else if (char === ']') {
      inClass = false;
    } else if (char === '/' && !inClass) {
      index += 1;
      while (index < source.length && /[A-Za-z]/.test(source[index])) index += 1;
      return index;
    } else if (char === '\n') {
      throw unbalanced('Unterminated JavaScript regular expression');
    }
  }
  throw unbalanced('Unterminated JavaScript regular expression');
}

function skipJavaScriptBracedExpression(source, start) {
  let depth = 1;
  for (let index = start; index < source.length;) {
    const char = source[index];
    if (char === '"' || char === "'") {
      index = skipQuoted(source, index, char);
    } else if (char === '`') {
      index = skipTemplateLiteral(source, index);
    } else if (source.startsWith('/*', index)) {
      index = skipBlockComment(source, index);
    } else if (source.startsWith('//', index)) {
      index = skipLineComment(source, index);
    } else if (char === '/' && canStartRegexLiteral(source, index)) {
      index = skipRegexLiteral(source, index);
    } else if (char === '{') {
      depth += 1;
      index += 1;
    } else if (char === '}') {
      depth -= 1;
      index += 1;
      if (depth === 0) return index;
    } else {
      index += 1;
    }
  }
  throw unbalanced('Unterminated JavaScript template expression');
}

function skipTemplateLiteral(source, start) {
  for (let index = start + 1; index < source.length;) {
    const char = source[index];
    if (char === '\\') {
      index += 2;
    } else if (char === '`') {
      return index + 1;
    } else if (char === '$' && source[index + 1] === '{') {
      index = skipJavaScriptBracedExpression(source, index + 2);
    } else {
      index += 1;
    }
  }
  throw unbalanced('Unterminated JavaScript template literal');
}

function skipHtmlComment(source, start) {
  const end = source.indexOf('-->', start + 4);
  if (end === -1) throw unbalanced('Unterminated HTML comment');
  return end + 3;
}

const HTML_RAW_TEXT_TAGS = new Set(['script', 'style', 'textarea', 'title']);

function findHtmlRawTextClose(source, tagName, start) {
  const lower = source.toLowerCase();
  const needle = `</${tagName.toLowerCase()}`;
  let index = lower.indexOf(needle, start);
  while (index !== -1) {
    const boundary = lower[index + needle.length];
    if (boundary === '>' || /\s/.test(boundary || '')) return index;
    index = lower.indexOf(needle, index + needle.length);
  }
  throw unbalanced(`Missing closing HTML raw-text tag for ${tagName}`);
}

function looksLikeTypeScriptGenericCall(source, tag) {
  const immediatelyBefore = source[tag.start - 1];
  if (immediatelyBefore && /[A-Za-z0-9_$.)\]]/.test(immediatelyBefore)) return true;

  let after = tag.end;
  while (source[after] === '>') after += 1;
  while (after < source.length && /\s/.test(source[after])) after += 1;
  if (source[after] === '('
    && (/,\s*>$/.test(tag.raw) || /\bextends\b/.test(tag.raw))) {
    return true;
  }
  let before = tag.start - 1;
  while (before >= 0 && /\s/.test(source[before])) before -= 1;
  if (source[after] === '('
    && /^[A-Z][A-Za-z0-9_$]*$/.test(tag.name)
    && /[=({;,:]/.test(source[before] || '')) {
    return true;
  }

  return false;
}

function scanTag(source, start) {
  let index = start + 1;
  let closing = false;
  if (source[index] === '/') {
    closing = true;
    index += 1;
  }
  const nameMatch = source.slice(index).match(/^[A-Za-z][A-Za-z0-9:._-]*/);
  if (!nameMatch) return null;
  const name = nameMatch[0];
  index += name.length;

  if (closing) {
    while (index < source.length && /\s/.test(source[index])) index += 1;
    if (source[index] !== '>') throw unbalanced('Malformed JSX closing tag');
    return {
      start,
      end: index + 1,
      name,
      closing: true,
      selfClosing: false,
      raw: source.slice(start, index + 1),
    };
  }

  let braceDepth = 0;

  while (index < source.length) {
    const char = source[index];
    if (char === '"' || char === "'") {
      index = skipQuoted(source, index, char);
      continue;
    }
    if (char === '`') {
      index = skipTemplateLiteral(source, index);
      continue;
    }
    if (braceDepth > 0 && source.startsWith('/*', index)) {
      index = skipBlockComment(source, index);
      continue;
    }
    if (braceDepth > 0 && source.startsWith('//', index)) {
      index = skipLineComment(source, index);
      continue;
    }
    if (braceDepth > 0
      && char === '/'
      && source[index - 1] !== '<'
      && canStartRegexLiteral(source, index)) {
      index = skipRegexLiteral(source, index);
      continue;
    }
    if (char === '{') {
      braceDepth += 1;
      index += 1;
      continue;
    }
    if (char === '}') {
      if (braceDepth === 0) throw unbalanced('Unexpected JSX attribute brace');
      braceDepth -= 1;
      index += 1;
      continue;
    }
    if (char === '>' && braceDepth === 0) {
      let tail = index - 1;
      while (tail > start && /\s/.test(source[tail])) tail -= 1;
      return {
        start,
        end: index + 1,
        name,
        closing,
        selfClosing: !closing && source[tail] === '/',
        raw: source.slice(start, index + 1),
      };
    }
    index += 1;
  }
  throw unbalanced('Unterminated JSX tag');
}

export function scanJsxTagAtOffset(source, offset) {
  if (!Number.isInteger(offset) || offset < 0 || offset >= source.length) return null;
  return source[offset] === '<' ? scanTag(source, offset) : null;
}

export function scanJsxTags(source, { htmlMode = false, stopAfterTag, stopWhen } = {}) {
  const tags = [];
  const lexicalJsxStack = [];
  let expressionDepth = 0;
  let htmlRawTextTag = null;
  let index = 0;
  while (index < source.length) {
    if (htmlMode && htmlRawTextTag) {
      index = findHtmlRawTextClose(source, htmlRawTextTag, index);
      htmlRawTextTag = null;
    }
    const char = source[index];
    const inJsxText = lexicalJsxStack.length > 0
      && lexicalJsxStack.at(-1).expressionDepth === expressionDepth;
    const inJavaScript = !htmlMode && !inJsxText;
    if (htmlMode && source.startsWith('<!--', index)) {
      index = skipHtmlComment(source, index);
      continue;
    }
    if (inJavaScript && (char === '"' || char === "'")) {
      index = skipQuoted(source, index, char);
      continue;
    }
    if (inJavaScript && char === '`') {
      index = skipTemplateLiteral(source, index);
      continue;
    }
    if (inJavaScript && source.startsWith('/*', index)) {
      index = skipBlockComment(source, index);
      continue;
    }
    if (inJavaScript && source.startsWith('//', index)) {
      index = skipLineComment(source, index);
      continue;
    }
    if (inJavaScript && char === '/' && canStartRegexLiteral(source, index)) {
      index = skipRegexLiteral(source, index);
      continue;
    }
    if (char === '{') {
      expressionDepth += 1;
      index += 1;
      continue;
    }
    if (char === '}') {
      if (expressionDepth === 0) throw unbalanced('Unexpected JSX expression brace');
      expressionDepth -= 1;
      index += 1;
      if (stopWhen?.({ expressionDepth, index, tags })) return tags;
      continue;
    }
    if (char === '<') {
      const tag = scanTag(source, index);
      if (tag) {
        const sameDepthParent = lexicalJsxStack.at(-1)?.expressionDepth === expressionDepth;
        const generic = !htmlMode
          && !tag.closing
          && !sameDepthParent
          && looksLikeTypeScriptGenericCall(source, tag);
        tags.push(tag);
        if (stopAfterTag?.(tag, tags, { expressionDepth })) return tags;
        if (!generic && !tag.selfClosing) {
          if (tag.closing) {
            if (lexicalJsxStack.at(-1)?.name === tag.name) lexicalJsxStack.pop();
          } else {
            lexicalJsxStack.push({ name: tag.name, expressionDepth });
          }
        }
        if (htmlMode
          && !tag.closing
          && !tag.selfClosing
          && HTML_RAW_TEXT_TAGS.has(tag.name.toLowerCase())) {
          htmlRawTextTag = tag.name;
        }
        index = tag.end;
        continue;
      }
    }
    index += 1;
  }
  if (expressionDepth !== 0) throw unbalanced('Unterminated JSX expression');
  return tags;
}

function scanTemplateForOffset(source, start, offset) {
  for (let index = start + 1; index < source.length;) {
    if (index >= offset) return { index, context: 'template', reachedOffset: true };
    const char = source[index];
    if (char === '\\') {
      if (index + 1 >= offset) return { index: offset, context: 'template', reachedOffset: true };
      index += 2;
    } else if (char === '`') {
      return { index: index + 1, context: 'code', reachedOffset: false };
    } else if (char === '$' && source[index + 1] === '{') {
      if (index + 2 > offset) return { index: offset, context: 'template', reachedOffset: true };
      const nested = scanCodeForTemplateOffset(source, index + 2, offset, true);
      if (nested.reachedOffset) return nested;
      index = nested.index;
    } else {
      index += 1;
    }
  }
  return { index: source.length, context: 'template', reachedOffset: offset <= source.length };
}

function scanCodeForTemplateOffset(source, start, offset, braced = false) {
  let depth = braced ? 1 : 0;
  for (let index = start; index < source.length;) {
    if (index >= offset) return { index, context: 'code', reachedOffset: true };
    const char = source[index];
    let next = index + 1;
    let skippedContext = 'code';
    if (char === '"' || char === "'") {
      next = skipQuoted(source, index, char);
      skippedContext = 'string';
    } else if (char === '`') {
      const template = scanTemplateForOffset(source, index, offset);
      if (template.reachedOffset) return template;
      next = template.index;
    } else if (source.startsWith('/*', index)) {
      next = skipBlockComment(source, index);
      skippedContext = 'comment';
    } else if (source.startsWith('//', index)) {
      next = skipLineComment(source, index);
      skippedContext = 'comment';
    } else if (char === '/' && canStartRegexLiteral(source, index)) {
      next = skipRegexLiteral(source, index);
      skippedContext = 'regex';
    } else if (braced && char === '{') {
      depth += 1;
    } else if (braced && char === '}') {
      depth -= 1;
      if (depth === 0) return { index: index + 1, context: 'code', reachedOffset: false };
    }
    if (next > offset) return { index: offset, context: skippedContext, reachedOffset: true };
    index = next;
  }
  return { index: source.length, context: 'code', reachedOffset: offset <= source.length };
}

export function isOffsetInsideJavaScriptTemplate(source, offset) {
  if (!Number.isInteger(offset) || offset < 0 || offset > source.length) return false;
  return scanCodeForTemplateOffset(source, 0, offset).context === 'template';
}

export function javascriptLexicalContextAtOffset(source, offset) {
  if (!Number.isInteger(offset) || offset < 0 || offset > source.length) return 'invalid';
  return scanCodeForTemplateOffset(source, 0, offset).context;
}

function htmlContextAtOffset(source, offset, includeExpressions) {
  if (!Number.isInteger(offset) || offset < 0 || offset > source.length) return 'invalid';
  let rawTextTag = null;
  for (let index = 0; index < source.length && index < offset;) {
    if (rawTextTag) {
      const close = findHtmlRawTextClose(source, rawTextTag, index);
      if (close >= offset) return 'raw-text';
      index = close;
      rawTextTag = null;
      continue;
    }
    if (source.startsWith('<!--', index)) {
      const end = skipHtmlComment(source, index);
      if (end > offset) return 'comment';
      index = end;
      continue;
    }
    if (includeExpressions && source[index] === '{') {
      const end = skipJavaScriptBracedExpression(source, index + 1);
      if (end > offset) return 'expression';
      index = end;
      continue;
    }
    if (source[index] === '<') {
      const tag = scanTag(source, index);
      if (tag) {
        if (!tag.closing
          && !tag.selfClosing
          && HTML_RAW_TEXT_TAGS.has(tag.name.toLowerCase())) {
          rawTextTag = tag.name;
        }
        index = tag.end;
        continue;
      }
    }
    index += 1;
  }
  return rawTextTag ? 'raw-text' : 'markup';
}

export function htmlLexicalContextAtOffset(source, offset) {
  return htmlContextAtOffset(source, offset, false);
}

export function frameworkTemplateContextAtOffset(source, offset) {
  return htmlContextAtOffset(source, offset, true);
}

export function isOffsetInsideAstroFrontmatter(source, offset) {
  const opening = source.match(/^(?:\uFEFF)?---[ \t]*\r?\n/);
  if (!opening) return false;
  const bodyStart = opening[0].length;
  const body = source.slice(bodyStart);
  const closingPattern = /^---[ \t]*(?:\r?\n|$)/gm;
  closingPattern.lastIndex = bodyStart;
  let closing = null;
  for (let candidate = closingPattern.exec(source); candidate; candidate = closingPattern.exec(source)) {
    if (javascriptLexicalContextAtOffset(body, candidate.index - bodyStart) === 'code') {
      closing = candidate;
      break;
    }
  }
  return !closing || offset < closing.index + closing[0].length;
}

export function hasExecutableJsxTagAtOffset(source, offset) {
  if (!Number.isInteger(offset) || offset < 0 || offset >= source.length) return false;
  let found = false;
  scanJsxTags(source, {
    stopAfterTag(tag) {
      if (tag.start < offset) return false;
      found = tag.start === offset;
      return true;
    },
  });
  return found;
}

export function hasExecutableJsxMarkerAtOffset(source, offset, markerLength) {
  if (!Number.isInteger(markerLength) || markerLength < 1) return false;
  const sentinel = '<ImpeccableLiveMarker />';
  const probe = source.slice(0, offset) + sentinel + source.slice(offset + markerLength);
  return hasExecutableJsxTagAtOffset(probe, offset);
}

export function findJsxSubtree(source, predicate, { strictNesting = true } = {}) {
  let opener = null;
  let openerExpressionDepth = null;
  let closing = null;
  const stack = [];
  let sameNameDepth = 0;
  const tags = scanJsxTags(source, {
    stopAfterTag(tag, _tags, { expressionDepth }) {
      if (!opener) {
        if (tag.closing || !predicate(tag)) return false;
        opener = tag;
        openerExpressionDepth = expressionDepth;
        if (tag.selfClosing) {
          closing = tag;
          return openerExpressionDepth === 0;
        }
        if (strictNesting) stack.push({ name: tag.name, expressionDepth });
        else sameNameDepth = 1;
        return false;
      }
      if (closing) return false;
      if (!strictNesting) {
        if (tag.name !== opener.name || tag.selfClosing) return false;
        if (tag.closing) sameNameDepth -= 1;
        else sameNameDepth += 1;
        if (sameNameDepth === 0) {
          closing = tag;
          return openerExpressionDepth === 0;
        }
        return false;
      }
      if (expressionDepth > openerExpressionDepth
        && !tag.closing
        && stack.at(-1)?.expressionDepth !== expressionDepth
        && looksLikeTypeScriptGenericCall(source, tag)) {
        return false;
      }
      if (tag.selfClosing) return false;
      if (!tag.closing) {
        stack.push({ name: tag.name, expressionDepth });
        return false;
      }
      const expected = stack.at(-1);
      if (!expected
        || expected.name !== tag.name
        || expected.expressionDepth !== expressionDepth) {
        throw unbalanced('Mismatched JSX closing tag');
      }
      stack.pop();
      if (stack.length === 0) {
        closing = tag;
        return openerExpressionDepth === 0;
      }
      return false;
    },
    stopWhen({ expressionDepth }) {
      return closing !== null && openerExpressionDepth > 0 && expressionDepth === 0;
    },
    htmlMode: !strictNesting,
  });
  if (!opener) throw unbalanced('Missing JSX opener');
  if (!closing) throw unbalanced(`Missing closing JSX tag for ${opener.name}`);
  return { opener, closing, tags };
}

export function findMatchingJsxTag(tags, openerIndex) {
  const opener = tags[openerIndex];
  if (!opener || opener.closing) throw unbalanced('Missing JSX opener');
  if (opener.selfClosing) return opener;
  let depth = 1;
  for (let index = openerIndex + 1; index < tags.length; index += 1) {
    const tag = tags[index];
    if (tag.name !== opener.name) continue;
    if (tag.closing) depth -= 1;
    else if (!tag.selfClosing) depth += 1;
    if (depth === 0) return tag;
  }
  throw unbalanced(`Missing closing JSX tag for ${opener.name}`);
}
