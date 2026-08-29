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

export function scanJsxTags(source, { stopAfterTag } = {}) {
  const tags = [];
  let expressionDepth = 0;
  let index = 0;
  while (index < source.length) {
    const char = source[index];
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
    if (char === '{') {
      expressionDepth += 1;
      index += 1;
      continue;
    }
    if (char === '}') {
      if (expressionDepth === 0) throw unbalanced('Unexpected JSX expression brace');
      expressionDepth -= 1;
      index += 1;
      continue;
    }
    if (char === '<') {
      const tag = scanTag(source, index);
      if (tag) {
        tags.push(tag);
        if (stopAfterTag?.(tag, tags)) return tags;
        index = tag.end;
        continue;
      }
    }
    index += 1;
  }
  if (expressionDepth !== 0) throw unbalanced('Unterminated JSX expression');
  return tags;
}

export function findJsxSubtree(source, predicate) {
  let opener = null;
  let closing = null;
  let depth = 0;
  const tags = scanJsxTags(source, {
    stopAfterTag(tag) {
      if (!opener) {
        if (tag.closing || !predicate(tag)) return false;
        opener = tag;
        if (tag.selfClosing) {
          closing = tag;
          return true;
        }
        depth = 1;
        return false;
      }
      if (tag.name !== opener.name) return false;
      if (tag.closing) depth -= 1;
      else if (!tag.selfClosing) depth += 1;
      if (depth === 0) {
        closing = tag;
        return true;
      }
      return false;
    },
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
