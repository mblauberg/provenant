// Modified from Impeccable for this harness; see the repository THIRD_PARTY_NOTICES.md.
/**
 * Shared context loader for every impeccable command that needs to know
 * "who is this for" and "what does this look like".
 *
 * Input: project root (process.cwd()).
 *
 * Library output (used by live tooling):
 *   {
 *     hasProduct: boolean,        // PRODUCT.md or legacy context found
 *     product: string | null,     // PRODUCT.md contents
 *     productPath: string | null, // relative path
 *     hasDesign: boolean,         // DESIGN.md found
 *     design: string | null,      // DESIGN.md contents
 *     designPath: string | null,
 *     migrated: boolean,          // retained for compatibility; always false
 *     contextDir: string,         // absolute path of the directory the files were found in
 *   }
 *
 * Filename matching is case-insensitive for PRODUCT.md and DESIGN.md. The
 * Google DESIGN.md convention is uppercase at repo root; Kiro-style and
 * lowercase variants are also matched so users don't get punished for case.
 *
 * Lookup directory resolution (first match wins):
 *   1. process.env.IMPECCABLE_CONTEXT_DIR (absolute or relative to cwd)
 *   2. cwd, if PRODUCT.md / DESIGN.md / .impeccable.md is there (back-compat)
 *   3. Auto-fallback subdirectories of cwd: .agents/context/, then docs/
 *   4. cwd as a default "no context found" location
 *
 * The CLI defaults to bounded metadata (paths, character counts and headings),
 * not full document bodies. `--max-chars N` adds bounded previews; `--full` is
 * an explicit diagnostic escape hatch. This loader is read-only. Legacy
 * `.impeccable.md` is read in place and any migration is left to an explicitly
 * authorised setup command.
 */

import fs from 'node:fs';
import path from 'node:path';

const PRODUCT_NAMES = ['PRODUCT.md', 'Product.md', 'product.md'];
const DESIGN_NAMES = ['DESIGN.md', 'Design.md', 'design.md'];
const LEGACY_NAMES = ['.impeccable.md'];
const FALLBACK_DIRS = ['.agents/context', 'docs'];
const SUMMARY_LIMITS = Object.freeze({
  maxHeadingsPerDocument: 20,
  maxHeadingTitleChars: 120,
  maxPathChars: 512,
  maxOutputChars: 10_000,
});

/**
 * Resolve the directory that holds PRODUCT.md / DESIGN.md for
 * this project. Exported so other scripts (e.g. live-server.mjs) can read the
 * design files from the same location the loader uses.
 */
export function resolveContextDir(cwd = process.cwd()) {
  // 1. Explicit override
  const envDir = process.env.IMPECCABLE_CONTEXT_DIR;
  if (envDir && envDir.trim()) {
    const trimmed = envDir.trim();
    return path.isAbsolute(trimmed) ? trimmed : path.resolve(cwd, trimmed);
  }

  // 2. cwd wins if any canonical or legacy file is there.
  if (firstExisting(cwd, [...PRODUCT_NAMES, ...DESIGN_NAMES, ...LEGACY_NAMES])) {
    return cwd;
  }

  // 3. Auto-fallback subdirs. Match if PRODUCT.md or DESIGN.md is present;
  //    legacy `.impeccable.md` does not pull the lookup into a fallback dir.
  for (const rel of FALLBACK_DIRS) {
    const candidate = path.resolve(cwd, rel);
    if (firstExisting(candidate, [...PRODUCT_NAMES, ...DESIGN_NAMES])) {
      return candidate;
    }
  }

  // 4. Nothing found — keep the historical "default to cwd" behaviour so the
  //    caller's `hasProduct === false` branch still fires the same way.
  return cwd;
}

export function loadContext(cwd = process.cwd()) {
  const contextDir = resolveContextDir(cwd);

  // 1. Look for PRODUCT.md (case-insensitive) in the resolved dir
  let productPath = firstExisting(contextDir, PRODUCT_NAMES);

  // 2. Legacy: read .impeccable.md in place. Loading context must never mutate
  //    a project; an authorised setup/migration command owns any rename.
  if (!productPath && contextDir === cwd) {
    const legacyPath = firstExisting(cwd, LEGACY_NAMES);
    if (legacyPath) productPath = legacyPath;
  }

  // 3. DESIGN.md (case-insensitive)
  const designPath = firstExisting(contextDir, DESIGN_NAMES);

  const product = productPath ? safeRead(productPath) : null;
  const design = designPath ? safeRead(designPath) : null;

  return {
    hasProduct: !!product,
    product,
    productPath: productPath ? path.relative(cwd, productPath) : null,
    hasDesign: !!design,
    design,
    designPath: designPath ? path.relative(cwd, designPath) : null,
    migrated: false,
    contextDir,
  };
}

function firstExisting(dir, names) {
  // Prefer the documented canonical/common spellings, then fall back to a
  // deterministic case-insensitive directory scan for case-sensitive hosts.
  for (const name of names) {
    const abs = path.join(dir, name);
    if (fs.existsSync(abs)) return abs;
  }
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return null;
  }
  const wanted = new Set(names.map((name) => name.toLowerCase()));
  const matches = entries
    .filter((entry) => entry.isFile() && wanted.has(entry.name.toLowerCase()))
    .map((entry) => entry.name)
    .sort((left, right) => left.localeCompare(right));
  if (matches.length) return path.join(dir, matches[0]);
  return null;
}

function safeRead(p) {
  try { return fs.readFileSync(p, 'utf-8'); } catch { return null; }
}

function truncateText(value, maxChars, preserveTail = false) {
  if (value == null || value.length <= maxChars) return { value, truncated: false };
  const characters = Array.from(value);
  if (characters.length <= maxChars) return { value, truncated: false };
  const kept = maxChars - 1;
  const clipped = preserveTail
    ? `…${characters.slice(-kept).join('')}`
    : `${characters.slice(0, kept).join('')}…`;
  return { value: clipped, truncated: true };
}

function headingSummary(value) {
  const items = [];
  let total = 0;
  let titlesTruncated = 0;
  if (value) {
    const pattern = /^\s{0,3}(#{1,6})[ \t]+([^\r\n]+)$/gm;
    for (const match of value.matchAll(pattern)) {
      total += 1;
      if (items.length >= SUMMARY_LIMITS.maxHeadingsPerDocument) continue;
      const title = match[2].replace(/[ \t]+#+[ \t]*$/, '').trim();
      const bounded = truncateText(title, SUMMARY_LIMITS.maxHeadingTitleChars);
      if (bounded.truncated) titlesTruncated += 1;
      items.push({ level: match[1].length, title: bounded.value });
    }
  }
  return {
    items,
    receipt: {
      total,
      returned: items.length,
      omitted: total - items.length,
      titlesTruncated,
    },
  };
}

function summary(result) {
  const productHeadings = headingSummary(result.product);
  const designHeadings = headingSummary(result.design);
  const productPath = truncateText(result.productPath, SUMMARY_LIMITS.maxPathChars, true);
  const designPath = truncateText(result.designPath, SUMMARY_LIMITS.maxPathChars, true);
  const contextDir = truncateText(result.contextDir, SUMMARY_LIMITS.maxPathChars, true);
  return {
    hasProduct: result.hasProduct,
    productPath: productPath.value,
    productChars: result.product?.length ?? 0,
    productHeadings: productHeadings.items,
    hasDesign: result.hasDesign,
    designPath: designPath.value,
    designChars: result.design?.length ?? 0,
    designHeadings: designHeadings.items,
    migrated: result.migrated,
    contextDir: contextDir.value,
    metadataLimits: SUMMARY_LIMITS,
    metadataTruncation: {
      productHeadings: productHeadings.receipt,
      designHeadings: designHeadings.receipt,
      paths: {
        productPath: productPath.truncated,
        designPath: designPath.truncated,
        contextDir: contextDir.truncated,
      },
      outputBudgetApplied: false,
    },
  };
}

export function summarizeContext(result) {
  const value = summary(result);
  let rendered = JSON.stringify(value, null, 2);
  while (rendered.length > SUMMARY_LIMITS.maxOutputChars
      && (value.productHeadings.length || value.designHeadings.length)) {
    const field = value.productHeadings.length >= value.designHeadings.length
      ? 'productHeadings'
      : 'designHeadings';
    value[field].pop();
    const receipt = value.metadataTruncation[field];
    receipt.returned = value[field].length;
    receipt.omitted = receipt.total - receipt.returned;
    value.metadataTruncation.outputBudgetApplied = true;
    rendered = JSON.stringify(value, null, 2);
  }
  if (rendered.length > SUMMARY_LIMITS.maxOutputChars) {
    // Paths are already individually capped. This fail-closed assertion keeps
    // future metadata fields from silently invalidating the total-output cap.
    throw new Error('Bounded context metadata exceeds maxOutputChars');
  }
  return value;
}

function boundedSummaryJson(result) {
  return JSON.stringify(summarizeContext(result), null, 2);
}

function boundedPreview(result, maxChars) {
  const value = summary(result);
  for (const field of ['product', 'design']) {
    const body = result[field];
    value[field] = body == null ? null : body.slice(0, maxChars);
    value[`${field}Truncated`] = body != null && body.length > maxChars;
  }
  return value;
}

// ---------------------------------------------------------------------------
// CLI mode — print the context as JSON
// ---------------------------------------------------------------------------

function cli() {
  const result = loadContext(process.cwd());
  const args = process.argv.slice(2);
  const full = args.includes('--full');
  const maxIndex = args.indexOf('--max-chars');
  if (full && maxIndex >= 0) {
    console.error('Use either --full or --max-chars, not both.');
    process.exitCode = 2;
    return;
  }
  if (full) {
    console.log(JSON.stringify(result, null, 2));
    return;
  }
  if (maxIndex >= 0) {
    const maxChars = Number.parseInt(args[maxIndex + 1] ?? '', 10);
    if (!Number.isInteger(maxChars) || maxChars < 1 || maxChars > 20000) {
      console.error('--max-chars must be an integer from 1 to 20000.');
      process.exitCode = 2;
      return;
    }
    console.log(JSON.stringify(boundedPreview(result, maxChars), null, 2));
    return;
  }
  console.log(boundedSummaryJson(result));
}

const _running = process.argv[1];
if (_running?.endsWith('load-context.mjs') || _running?.endsWith('load-context.mjs/')) {
  cli();
}
