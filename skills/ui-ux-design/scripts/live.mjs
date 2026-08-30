#!/usr/bin/env node
// Modified for Provenant.

import { pathToFileURL } from 'node:url';
import { resolveUiLiveScript } from './ui-live-paths.mjs';

await import(pathToFileURL(resolveUiLiveScript('live.mjs')));
