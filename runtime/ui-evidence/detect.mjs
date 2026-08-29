#!/usr/bin/env node

import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { detectCli } from './detector/detect-antipatterns.mjs';

export * from './detector/detect-antipatterns.mjs';

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await detectCli();
}
