#!/usr/bin/env node

import {
  existsSync,
  lstatSync,
  readFileSync,
  realpathSync,
  statSync,
} from "node:fs";
import { isAbsolute, join, relative, sep } from "node:path";


const checkout = realpathSync(process.cwd());
const rootPackage = readLocalManifest(join(checkout, "package.json"), "root manifest");
const workspacePaths = Array.isArray(rootPackage.workspaces)
  ? rootPackage.workspaces
  : [];
const manifests = [
  [checkout, rootPackage],
  ...workspacePaths.map((workspace) => {
    const workspaceRoot = localWorkspace(join(checkout, workspace));
    return [
      workspaceRoot,
      readLocalManifest(join(workspaceRoot, "package.json"), "workspace manifest"),
    ];
  }),
];

const missing = new Set();
for (const [manifestRoot, manifest] of manifests) {
  const dependencies = {
    ...(manifest.dependencies ?? {}),
    ...(manifest.devDependencies ?? {}),
  };
  for (const dependency of Object.keys(dependencies)) {
    const packagePath = dependency.split("/");
    const candidates = [
      join(manifestRoot, "node_modules", ...packagePath),
      join(checkout, "node_modules", ...packagePath),
    ];
    if (!candidates.some((candidate) => isLocalPackage(candidate, dependency))) {
      missing.add(dependency);
    }
  }
}

if (missing.size > 0) {
  process.stderr.write(
    `node-workspace-preflight: missing checkout dependencies for ${checkout}\n`
    + `node-workspace-preflight: missing ${[...missing].sort().join(", ")}\n`
    + "node-workspace-preflight: run `npm ci` from this checkout\n",
  );
  process.exitCode = 3;
}


function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}


function readLocalManifest(path, label) {
  try {
    if (!lstatSync(path).isFile() || !isInsideCheckout(realpathSync(path))) {
      throw new Error("not local");
    }
    return readJson(path);
  } catch {
    process.stderr.write(
      `node-workspace-preflight: ${label} is not a local regular file: ${path}\n`,
    );
    process.exit(3);
  }
}


function localWorkspace(path) {
  try {
    const resolved = realpathSync(path);
    if (!isInsideCheckout(resolved) || !statSync(resolved).isDirectory()) {
      throw new Error("not local");
    }
    return resolved;
  } catch {
    process.stderr.write(
      `node-workspace-preflight: workspace is not a local directory: ${path}\n`,
    );
    process.exit(3);
  }
}


function isLocalPackage(packageDirectory, expectedName) {
  if (!existsSync(packageDirectory)) {
    return false;
  }
  try {
    const resolvedDirectory = realpathSync(packageDirectory);
    if (!isInsideCheckout(resolvedDirectory) || !statSync(resolvedDirectory).isDirectory()) {
      return false;
    }
    const packageFile = join(resolvedDirectory, "package.json");
    if (!lstatSync(packageFile).isFile()) {
      return false;
    }
    return readJson(packageFile).name === expectedName;
  } catch {
    return false;
  }
}


function isInsideCheckout(path) {
  const offset = relative(checkout, path);
  return offset !== ""
    && offset !== ".."
    && !offset.startsWith(`..${sep}`)
    && !isAbsolute(offset);
}
