import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const RUNTIME_RELATIVE = path.join('runtime', 'ui-evidence');

function runtimeAt(productRoot) {
  return path.join(productRoot, RUNTIME_RELATIVE);
}

function requireRuntime(productRoot, source) {
  const runtimeRoot = runtimeAt(productRoot);
  if (!fs.existsSync(runtimeRoot) || !fs.statSync(runtimeRoot).isDirectory()) {
    throw new Error(
      `UI evidence runtime not found under ${source} ${productRoot}; expected ${runtimeRoot}`,
    );
  }
  return runtimeRoot;
}

function configuredProductRoot() {
  for (const name of ['AGENT_FABRIC_PRODUCT_ROOT', 'AGENTS_HOME']) {
    const value = process.env[name]?.trim();
    if (!value) continue;
    if (!path.isAbsolute(value)) {
      throw new Error(`${name} must be an absolute product root, got ${value}`);
    }
    return { name, value };
  }
  return null;
}

export function resolveUiEvidenceRoot() {
  const configured = configuredProductRoot();
  if (configured) {
    return requireRuntime(configured.value, configured.name);
  }

  const physicalSource = fs.realpathSync.native(fileURLToPath(import.meta.url));
  const productRoot = path.resolve(path.dirname(physicalSource), '..', '..', '..');
  return requireRuntime(productRoot, 'the physical skill checkout at');
}

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
