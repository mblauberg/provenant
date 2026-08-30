import path from 'node:path';
import { resolveUiRuntimeRoot } from './ui-runtime-paths.mjs';

export const resolveUiLiveRoot = () => resolveUiRuntimeRoot('ui-live');

export function resolveUiLiveScript(name) {
  if (!/^[a-z][a-z0-9-]*\.mjs$/.test(name)) {
    throw new Error(`Invalid UI live runtime script name: ${name}`);
  }
  return path.join(resolveUiLiveRoot(), name);
}
