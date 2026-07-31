import { existsSync, readFileSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";

import { isInstanceRootPaired, type FabricRoots } from "../domain/fabric-roots.js";

const PRODUCT_ROOT_POINTER = ".agent-fabric/product-root.json";

export type RootPairingDependencies = {
  /** Injected for tests; production reads the machine-local pointer. */
  readFile?: ((path: string) => string) | undefined;
  /** Injected for tests; production rejects pointers to missing products. */
  exists?: ((path: string) => boolean) | undefined;
};

export function readProductRootPointer(
  instanceRoot: string,
  dependencies: RootPairingDependencies = {},
): string | undefined {
  const readFile = dependencies.readFile ?? ((path: string) => readFileSync(path, "utf8"));
  const exists = dependencies.exists ?? existsSync;
  try {
    const value: unknown = JSON.parse(readFile(join(instanceRoot, PRODUCT_ROOT_POINTER)));
    if (
      typeof value !== "object" || value === null || Array.isArray(value) ||
      !("schema_version" in value) || value.schema_version !== 1 ||
      !("product_root" in value) || typeof value.product_root !== "string" ||
      !value.product_root || !isAbsolute(value.product_root)
    ) {
      return undefined;
    }
    const productRoot = resolve(value.product_root);
    return exists(productRoot) ? productRoot : undefined;
  } catch {
    return undefined;
  }
}

export function hasPairedInstanceRoot(
  roots: FabricRoots,
  dependencies: RootPairingDependencies = {},
): boolean {
  return isInstanceRootPaired(roots, readProductRootPointer(roots.instanceRoot, dependencies));
}
