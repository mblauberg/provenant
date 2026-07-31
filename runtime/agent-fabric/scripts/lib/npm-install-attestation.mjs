import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { lstat, opendir, readFile, readlink, rename, writeFile } from "node:fs/promises";
import { join, relative, sep } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);

export const NPM_INSTALL_ATTESTATION_ERROR = "NPM_INSTALL_ATTESTATION_MISMATCH";
export const NPM_INSTALL_ATTESTATION_RELATIVE_PATH = join("runtime", "agent-fabric", ".npm-ci-attestation");
export const NPM_INSTALL_RECOVERY_COMMAND = "scripts/install-agent-fabric-dependencies";

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

export async function sha256File(path) {
  return sha256(await readFile(path));
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export async function readPackageSriValues(lockfilePath) {
  const document = JSON.parse(await readFile(lockfilePath, "utf8"));
  if (!isRecord(document) || !isRecord(document.packages)) {
    throw new TypeError(`npm lockfile has no packages map: ${lockfilePath}`);
  }
  const values = {};
  for (const packagePath of Object.keys(document.packages).sort()) {
    const entry = document.packages[packagePath];
    if (!isRecord(entry) || entry.integrity === undefined) continue;
    if (typeof entry.integrity !== "string" || entry.integrity.length === 0) {
      throw new TypeError(`npm lockfile package has an invalid integrity value: ${packagePath}`);
    }
    values[packagePath] = entry.integrity;
  }
  return values;
}

function canonicalRelativePath(root, path) {
  return relative(root, path).split(sep).join("/");
}

async function collectInstalledEntries(root, directory, entries) {
  const handle = await opendir(directory);
  for await (const directoryEntry of handle) {
    const path = join(directory, directoryEntry.name);
    const relativePath = canonicalRelativePath(root, path);
    if (relativePath === ".bin" || relativePath.startsWith(".bin/")) continue;
    const metadata = await lstat(path);
    if (metadata.isDirectory()) {
      await collectInstalledEntries(root, path, entries);
    } else if (metadata.isFile()) {
      entries.push({ path: relativePath, kind: "file", sha256: await sha256File(path) });
    } else if (metadata.isSymbolicLink()) {
      entries.push({ path: relativePath, kind: "symlink", sha256: sha256(await readlink(path)) });
    } else {
      throw new TypeError(`node_modules contains an unsupported filesystem entry: ${relativePath}`);
    }
  }
}

export async function installedTreeSha256(nodeModulesPath) {
  const entries = [];
  await collectInstalledEntries(nodeModulesPath, nodeModulesPath, entries);
  entries.sort((left, right) => left.path < right.path ? -1 : left.path > right.path ? 1 : 0);
  const digest = createHash("sha256");
  for (const entry of entries) {
    digest.update(`${JSON.stringify([entry.path, entry.kind, entry.sha256])}\n`);
  }
  return digest.digest("hex");
}

async function productCommit(productRoot) {
  const result = await execFileAsync("git", ["-C", productRoot, "rev-parse", "HEAD"], { encoding: "utf8" });
  return result.stdout.trim();
}

export async function createNpmInstallAttestation(productRoot) {
  const lockfilePath = join(productRoot, "package-lock.json");
  return {
    productCommit: await productCommit(productRoot),
    lockfileSha256: await sha256File(lockfilePath),
    packageSriValues: await readPackageSriValues(lockfilePath),
    installedTreeSha256: await installedTreeSha256(join(productRoot, "node_modules")),
  };
}

export async function writeNpmInstallAttestation(productRoot) {
  const path = join(productRoot, NPM_INSTALL_ATTESTATION_RELATIVE_PATH);
  const temporaryPath = `${path}.tmp-${process.pid}`;
  const attestation = await createNpmInstallAttestation(productRoot);
  await writeFile(temporaryPath, `${JSON.stringify(attestation, null, 2)}\n`, { encoding: "utf8", mode: 0o600 });
  await rename(temporaryPath, path);
  return path;
}

function parseAttestation(value) {
  if (
    !isRecord(value) ||
    typeof value.productCommit !== "string" ||
    typeof value.lockfileSha256 !== "string" ||
    !isRecord(value.packageSriValues) ||
    typeof value.installedTreeSha256 !== "string" ||
    !Object.values(value.packageSriValues).every((entry) => typeof entry === "string")
  ) {
    throw new TypeError("npm install attestation has an invalid shape");
  }
  return value;
}

export async function checkNpmInstallAttestation(productRoot) {
  const recovery = join(productRoot, NPM_INSTALL_RECOVERY_COMMAND);
  const attestationPath = join(productRoot, NPM_INSTALL_ATTESTATION_RELATIVE_PATH);
  let attestation;
  try {
    attestation = parseAttestation(JSON.parse(await readFile(attestationPath, "utf8")));
  } catch (error) {
    const reason = isRecord(error) && error.code === "ENOENT" ? "missing" : "invalid";
    const detail = reason === "missing" ? "npm install attestation is missing" : "npm install attestation is invalid";
    return { reason, message: `${detail}; rerun: ${recovery}`, cause: error };
  }
  let commit;
  try {
    commit = await productCommit(productRoot);
  } catch (error) {
    return {
      reason: "product-commit",
      message: `product commit cannot be verified against npm install attestation; rerun: ${recovery}`,
      cause: error,
    };
  }
  if (attestation.productCommit !== commit) {
    return {
      reason: "product-commit",
      message: `product commit changed after npm ci; rerun: ${recovery}`,
    };
  }
  const lockfilePath = join(productRoot, "package-lock.json");
  let lockfileDigest;
  try {
    lockfileDigest = await sha256File(lockfilePath);
  } catch (error) {
    return { reason: "lockfile", message: `npm lockfile cannot be verified; rerun: ${recovery}`, cause: error };
  }
  if (attestation.lockfileSha256 !== lockfileDigest) {
    return { reason: "lockfile", message: `package-lock.json was modified after npm ci; rerun: ${recovery}` };
  }
  let sriValues;
  try {
    sriValues = await readPackageSriValues(lockfilePath);
  } catch (error) {
    return { reason: "package-sri", message: `npm package SRI values cannot be verified; rerun: ${recovery}`, cause: error };
  }
  if (JSON.stringify(attestation.packageSriValues) !== JSON.stringify(sriValues)) {
    return { reason: "package-sri", message: `npm package SRI values changed after npm ci; rerun: ${recovery}` };
  }
  let treeDigest;
  try {
    treeDigest = await installedTreeSha256(join(productRoot, "node_modules"));
  } catch (error) {
    return { reason: "installed-tree", message: `installed npm tree cannot be verified; rerun: ${recovery}`, cause: error };
  }
  if (attestation.installedTreeSha256 !== treeDigest) {
    return { reason: "installed-tree", message: `node_modules was modified after npm ci; rerun: ${recovery}` };
  }
  return undefined;
}
