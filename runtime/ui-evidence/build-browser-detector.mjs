#!/usr/bin/env node
// Modified for Provenant.

import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const runtimeRoot = path.dirname(fileURLToPath(import.meta.url));
const outputRelativePath = 'detector/detect-antipatterns-browser.js';

const sources = [
  'detector/shared/constants.mjs',
  'detector/registry/antipatterns.mjs',
  'detector/shared/color.mjs',
  'detector/rules/checks.mjs',
  'detector/browser/injected/index.mjs',
];

const header = `/**
 * Anti-Pattern Browser Detector for Impeccable
 * Copyright (c) 2026 Paul Bakaus
 * SPDX-License-Identifier: Apache-2.0
 *
 * GENERATED -- do not edit. Sources:
 *   detector/shared/constants.mjs, detector/registry/antipatterns.mjs,
 *   detector/shared/color.mjs, detector/rules/checks.mjs, and
 *   detector/browser/injected/index.mjs.
 * Rebuild: node runtime/ui-evidence/build-browser-detector.mjs
 *
 * Modified for Provenant.
 *
 * Usage: <script src="detect-antipatterns-browser.js"></script>
 * Re-scan: window.impeccableScan()
 */
`;

const checksImports = `import {
  BORDER_SAFE_TAGS,
  GENERIC_FONTS,
  KNOWN_SERIF_FONTS,
  OVERUSED_FONTS,
  SAFE_TAGS,
  WCAG_LARGE_BOLD_TEXT_PX,
  WCAG_LARGE_TEXT_PX,
  isBrandFontOnOwnDomain,
} from '../shared/constants.mjs';
import {
  colorToHex,
  contrastRatio,
  getHue,
  hasChroma,
  isNeutralColor,
  parseGradientColors,
  parseRgb,
  relativeLuminance,
} from '../shared/color.mjs';
`;

const terminalNamedExport = /\nexport \{\n(?:  [A-Za-z_$][\w$]*,\n)+\};\s*$/;
const esmDeclaration = /^\s*(?:import|export)\b/m;

function fail(relativePath, detail) {
  throw new Error(`Cannot build browser detector: ${relativePath} ${detail}`);
}

function stripTerminalNamedExport(source, relativePath) {
  const match = source.match(terminalNamedExport);
  if (!match) fail(relativePath, 'must end with a named export block');
  return source.slice(0, match.index);
}

function assertNoEsmDeclarations(source, relativePath) {
  if (esmDeclaration.test(source)) {
    fail(relativePath, 'contains an unsupported ESM declaration');
  }
}

function browserRegistry(source, relativePath) {
  if (!source.startsWith('const ANTIPATTERNS = [')) {
    fail(relativePath, 'must begin with the ANTIPATTERNS declaration');
  }
  const boundary = '\n];\n\nconst RULE_ENGINE_SUPPORT = {';
  const end = source.indexOf(boundary);
  if (end === -1) {
    fail(relativePath, 'is missing the browser subset boundary');
  }
  const browserSource = source.slice(0, end + 3);
  assertNoEsmDeclarations(browserSource, relativePath);
  return browserSource;
}

function browserChecks(source, relativePath) {
  if (!source.startsWith(checksImports)) {
    fail(relativePath, 'imports must match the supported local browser dependencies');
  }
  return stripTerminalNamedExport(source.slice(checksImports.length), relativePath);
}

function sourceForBrowser(relativePath, source) {
  if (relativePath === 'detector/registry/antipatterns.mjs') {
    return browserRegistry(source, relativePath);
  }
  if (relativePath === 'detector/rules/checks.mjs') {
    return browserChecks(source, relativePath);
  }
  if (relativePath === 'detector/browser/injected/index.mjs') {
    assertNoEsmDeclarations(source, relativePath);
    return source;
  }
  const browserSource = stripTerminalNamedExport(source, relativePath);
  assertNoEsmDeclarations(browserSource, relativePath);
  return browserSource;
}

export async function renderBrowserDetector({ runtimeRoot: root = runtimeRoot } = {}) {
  const sections = await Promise.all(sources.map(async relativePath => {
    const source = await readFile(path.join(root, relativePath), 'utf8');
    return `// --- ${relativePath} ---\n${sourceForBrowser(relativePath, source).trim()}\n\n`;
  }));

  return `${header}(function () {\nif (typeof window === 'undefined') return;\n${sections.join('')}})();\n`;
}

export async function checkBrowserDetector({ runtimeRoot: root = runtimeRoot } = {}) {
  const output = path.join(root, outputRelativePath);
  const [rendered, existing] = await Promise.all([
    renderBrowserDetector({ runtimeRoot: root }),
    readFile(output, 'utf8'),
  ]);
  if (rendered !== existing) {
    throw new Error(
      `browser detector is out of date: run node runtime/ui-evidence/build-browser-detector.mjs`,
    );
  }
}

export async function buildBrowserDetector({ runtimeRoot: root = runtimeRoot } = {}) {
  await writeFile(
    path.join(root, outputRelativePath),
    await renderBrowserDetector({ runtimeRoot: root }),
  );
}

async function main(args) {
  if (args.length === 0) {
    await buildBrowserDetector();
    return;
  }
  if (args.length === 1 && args[0] === '--check') {
    await checkBrowserDetector();
    return;
  }
  throw new Error('Usage: node runtime/ui-evidence/build-browser-detector.mjs [--check]');
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  await main(process.argv.slice(2));
}
