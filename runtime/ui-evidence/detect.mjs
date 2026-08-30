#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { detectCli } from './detector/detect-antipatterns.mjs';

export * from './detector/detect-antipatterns.mjs';

function isMainModule() {
  if (!process.argv[1]) return false;
  try {
    return fs.realpathSync.native(path.resolve(process.argv[1]))
      === fs.realpathSync.native(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (isMainModule()) {
  await detectCli();
}
