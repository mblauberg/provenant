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
const rootPackage = readPackage(join(checkout, "package.json"));
const workspacePaths = Array.isArray(rootPackage.workspaces)
  ? rootPackage.workspaces
  : [];
const manifests = [
  [checkout, rootPackage],
  ...workspacePaths.map((workspace) => {
    const workspaceRoot = join(checkout, workspace);
    return [workspaceRoot, readPackage(join(workspaceRoot, "package.json"))];
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


function readPackage(path) {
  return JSON.parse(readFileSync(path, "utf8"));
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
    return readPackage(packageFile).name === expectedName;
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
