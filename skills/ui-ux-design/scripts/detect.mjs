#!/usr/bin/env node
// Modified for Provenant.

import { pathToFileURL } from 'node:url';

import { resolveUiEvidenceEntry } from './ui-evidence-paths.mjs';

let detector;
try {
  detector = await import(pathToFileURL(resolveUiEvidenceEntry()));
} catch (error) {
  process.stderr.write(`Error: ${error.message}\n`);
  process.exit(1);
}

await detector.detectCli();
