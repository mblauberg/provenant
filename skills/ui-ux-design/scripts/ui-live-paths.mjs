import path from 'node:path';
import { resolveUiRuntimeRoot } from './ui-runtime-paths.mjs';

export const resolveUiLiveRoot = () => resolveUiRuntimeRoot('ui-live');

export function resolveUiLiveScript(name) {
  return path.join(resolveUiLiveRoot(), name);
}
