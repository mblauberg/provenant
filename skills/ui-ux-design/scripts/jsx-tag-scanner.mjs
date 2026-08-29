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

function canStartRegexLiteral(source, start) {
  let before = start - 1;
  while (before >= 0 && /\s/.test(source[before])) before -= 1;
  if (before < 0 || /[=(:,\[{!&|?;+*%^~<>]/.test(source[before])) return true;
  return /\b(?:await|case|delete|in|instanceof|return|throw|typeof|void|yield)\s*$/.test(
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
    if (char === '"' || char === "'" || char === '`') {
      index = skipQuoted(source, index, char);
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

export function scanJsxTags(source, { htmlMode = false, stopAfterTag, stopWhen } = {}) {
  const tags = [];
  let expressionDepth = 0;
  let htmlRawTextTag = null;
  let index = 0;
  while (index < source.length) {
    if (htmlMode && htmlRawTextTag) {
      index = findHtmlRawTextClose(source, htmlRawTextTag, index);
      htmlRawTextTag = null;
    }
    const char = source[index];
    if (htmlMode && source.startsWith('<!--', index)) {
      index = skipHtmlComment(source, index);
      continue;
    }
    if (expressionDepth > 0 && (char === '"' || char === "'" || char === '`')) {
      index = skipQuoted(source, index, char);
      continue;
    }
    if (expressionDepth > 0 && source.startsWith('/*', index)) {
      index = skipBlockComment(source, index);
      continue;
    }
    if (expressionDepth > 0 && source.startsWith('//', index)) {
      index = skipLineComment(source, index);
      continue;
    }
    if (expressionDepth > 0 && char === '/' && canStartRegexLiteral(source, index)) {
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
        tags.push(tag);
        if (stopAfterTag?.(tag, tags, { expressionDepth })) return tags;
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
