import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const RUNTIMES = new Set(['ui-evidence', 'ui-live']);

function requireRuntime(productRoot, runtime, source) {
  const runtimeRoot = path.join(productRoot, 'runtime', runtime);
  if (!fs.existsSync(runtimeRoot) || !fs.statSync(runtimeRoot).isDirectory()) {
    throw new Error(
      `${runtime} runtime not found under ${source} ${productRoot}; expected ${runtimeRoot}`,
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

export function resolveUiRuntimeRoot(runtime) {
  if (!RUNTIMES.has(runtime)) throw new Error(`Unknown UI runtime: ${runtime}`);

  const configured = configuredProductRoot();
  if (configured) return requireRuntime(configured.value, runtime, configured.name);

  const physicalSource = fs.realpathSync.native(fileURLToPath(import.meta.url));
  const productRoot = path.resolve(path.dirname(physicalSource), '..', '..', '..');
  return requireRuntime(productRoot, runtime, 'the physical skill checkout at');
}
