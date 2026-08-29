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

function configuredProductRoot(env) {
  for (const name of ['AGENT_FABRIC_PRODUCT_ROOT', 'AGENTS_HOME']) {
    const value = env[name]?.trim();
    if (!value) continue;
    if (!path.isAbsolute(value)) {
      throw new Error(`${name} must be an absolute product root, got ${value}`);
    }
    return { name, value };
  }
  return null;
}

export function resolveUiEvidenceRoot({ env = process.env, sourceUrl = import.meta.url } = {}) {
  const configured = configuredProductRoot(env);
  if (configured) {
    return requireRuntime(configured.value, configured.name);
  }

  const physicalSource = fs.realpathSync.native(fileURLToPath(sourceUrl));
  const productRoot = path.resolve(path.dirname(physicalSource), '..', '..', '..');
  return requireRuntime(productRoot, 'the physical skill checkout at');
}

export function resolveUiEvidenceEntry(options) {
  return path.join(resolveUiEvidenceRoot(options), 'detect.mjs');
}

export function resolveUiEvidenceBrowserBundle(options) {
  return path.join(
    resolveUiEvidenceRoot(options),
    'detector',
    'detect-antipatterns-browser.js',
  );
}
