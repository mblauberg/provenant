#!/usr/bin/env node
// Modified for Provenant.

import { pathToFileURL } from 'node:url';

import { resolveUiEvidenceEntry } from './ui-evidence-paths.mjs';

try {
  const { detectCli } = await import(pathToFileURL(resolveUiEvidenceEntry()));
  await detectCli();
} catch (error) {
  process.stderr.write(`Error: ${error.message}\n`);
  process.exit(1);
}
