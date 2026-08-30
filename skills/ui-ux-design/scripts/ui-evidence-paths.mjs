import path from 'node:path';
import { resolveUiRuntimeRoot } from './ui-runtime-paths.mjs';

export const resolveUiEvidenceRoot = () => resolveUiRuntimeRoot('ui-evidence');

export function resolveUiEvidenceEntry() {
  return path.join(resolveUiEvidenceRoot(), 'detect.mjs');
}

export function resolveUiEvidenceBrowserBundle() {
  return path.join(
    resolveUiEvidenceRoot(),
    'detector',
    'detect-antipatterns-browser.js',
  );
}
