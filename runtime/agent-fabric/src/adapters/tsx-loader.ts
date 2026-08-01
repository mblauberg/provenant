import { readFile, realpath } from "node:fs/promises";
import { isAbsolute, join, relative, resolve } from "node:path";

import { FabricError } from "../errors.js";

function isWithin(path: string, root: string): boolean {
  const distance = relative(root, path);
  return distance.length === 0 || (!distance.startsWith("..") && !isAbsolute(distance));
}

/**
 * Admit the exact loader bytes that runtime wrappers are allowed to execute.
 * The package declaration boundary is the installed product's node_modules;
 * the resolved loader must remain inside that boundary and its owning package
 * must identify itself as tsx.
 */
export async function admitTsxLoader(input: {
  loaderPath: string;
  productRoot: string;
}): Promise<string> {
  if (!isAbsolute(input.loaderPath)) {
    throw new FabricError("ADAPTER_COMPATIBILITY_INVALID", "tsx loader path must be absolute");
  }
  try {
    const modulesRoot = await realpath(join(input.productRoot, "node_modules"));
    const resolvedLoader = await realpath(input.loaderPath);
    const packageRoot = resolve(resolvedLoader, "..", "..");
    if (!isWithin(resolvedLoader, modulesRoot) || !resolvedLoader.endsWith(join("tsx", "dist", "loader.mjs"))) {
      throw new Error("loader target is outside the admitted tsx package");
    }
    const manifest = JSON.parse(await readFile(join(packageRoot, "package.json"), "utf8")) as { name?: unknown };
    if (manifest.name !== "tsx") throw new Error("loader package is not tsx");
    return resolvedLoader;
  } catch (error: unknown) {
    throw new FabricError(
      "ADAPTER_COMPATIBILITY_INVALID",
      `tsx loader is not admitted: ${input.loaderPath}`,
      { cause: error },
    );
  }
}
