#!/usr/bin/env node

import {
  existsSync,
  readFileSync,
  realpathSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";


const checkout = realpathSync(resolve(dirname(fileURLToPath(import.meta.url)), ".."));
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
      join(manifestRoot, "node_modules", ...packagePath, "package.json"),
      join(checkout, "node_modules", ...packagePath, "package.json"),
    ];
    if (!candidates.some(isLocalPackage)) {
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


function isLocalPackage(path) {
  if (!existsSync(path)) {
    return false;
  }
  const resolved = realpathSync(path);
  const offset = relative(checkout, resolved);
  return offset !== ""
    && offset !== ".."
    && !offset.startsWith(`..${sep}`)
    && !isAbsolute(offset);
}
