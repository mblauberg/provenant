import { existsSync, readFileSync, realpathSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";

import { isInstanceRootPaired, type FabricRoots } from "../domain/fabric-roots.js";

const PRODUCT_ROOT_POINTER = ".agent-fabric/product-root.json";

export type RootPairingDependencies = {
  /** Injected for tests; production reads the machine-local pointer. */
  readFile?: ((path: string) => string) | undefined;
  /** Injected for tests; production rejects pointers to missing products. */
  exists?: ((path: string) => boolean) | undefined;
  /** Injected for tests; production follows symlinks to compare real paths. */
  realpath?: ((path: string) => string) | undefined;
};

export class ProductPointerError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ProductPointerError";
  }
}

/**
 * Both sides of the pairing comparison have to be canonical, and neither is
 * canonical by construction: the pointer is written by `install-harness` from
 * `resolve(strict=True)`, whereas a product root taken from `--agents-home`,
 * `AGENT_FABRIC_PRODUCT_ROOT` or `AGENTS_HOME` is only lexically resolved.
 * Comparing those two directly reports a genuinely paired instance as unpaired
 * whenever the product is reached through a symlink. A path that cannot be
 * canonicalised does not exist, which is never a pairing.
 */
function canonicalise(
  path: string,
  realpath: (candidate: string) => string,
): string | undefined {
  try {
    return realpath(path);
  } catch {
    return undefined;
  }
}

function isMissingFile(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error
    && (error as { code?: unknown }).code === "ENOENT";
}

/**
 * Absent and untrustworthy are different answers.
 *
 * A local layer can only narrow: allow-lists intersect, workspace roots must
 * stay contained and limits take the minimum. So quietly deciding "no local
 * layer" WIDENS the effective configuration. An instance with no pointer is an
 * ordinary unpaired instance and says so quietly, but a pointer that exists and
 * cannot be trusted must not be downgraded into silence, because that failure
 * would loosen the very limits the file exists to impose. The pointer is
 * rewritten on every install, so the recovery is always `install-harness`.
 */
export function readProductRootPointer(
  instanceRoot: string,
  dependencies: RootPairingDependencies = {},
): string | undefined {
  const readFile = dependencies.readFile ?? ((path: string) => readFileSync(path, "utf8"));
  const exists = dependencies.exists ?? existsSync;
  const realpath = dependencies.realpath ?? realpathSync;
  const pointerPath = join(instanceRoot, PRODUCT_ROOT_POINTER);
  let raw: string;
  try {
    raw = readFile(pointerPath);
  } catch (error) {
    if (isMissingFile(error)) {
      return undefined;
    }
    throw new ProductPointerError(
      `product pointer ${pointerPath} is unreadable: ${String(error)}. `
        + "Re-run install-harness to rewrite it.",
    );
  }
  let value: unknown;
  try {
    value = JSON.parse(raw);
  } catch (error) {
    throw new ProductPointerError(
      `product pointer ${pointerPath} is not readable JSON: ${String(error)}. `
        + "Re-run install-harness to rewrite it.",
    );
  }
  if (
    typeof value !== "object" || value === null || Array.isArray(value) ||
    !("schema_version" in value) || value.schema_version !== 1 ||
    !("product_root" in value) || typeof value.product_root !== "string" ||
    !value.product_root || !isAbsolute(value.product_root)
  ) {
    throw new ProductPointerError(
      `product pointer ${pointerPath} does not carry schema_version 1 with an `
        + "absolute product_root. Re-run install-harness to rewrite it.",
    );
  }
  const productRoot = resolve(value.product_root);
  // A pointer naming a product that is no longer on this machine is stale
  // rather than corrupt, which is the ordinary state after moving a checkout.
  return exists(productRoot) ? canonicalise(productRoot, realpath) : undefined;
}

export function hasPairedInstanceRoot(
  roots: FabricRoots,
  dependencies: RootPairingDependencies = {},
): boolean {
  const realpath = dependencies.realpath ?? realpathSync;
  return isInstanceRootPaired(
    canonicalise(roots.productRoot, realpath),
    readProductRootPointer(roots.instanceRoot, dependencies),
  );
}
