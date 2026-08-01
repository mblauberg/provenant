import { createHash, randomBytes } from "node:crypto";
import Database from "better-sqlite3";
import { constants } from "node:fs";
import { chmod, lstat, open, readFile, readdir, realpath, rename, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, parse, relative, resolve, sep } from "node:path";

import { ensureFabricPaths, type FabricPaths } from "./paths.js";

const PROFILE_PATTERN = /^[a-z][a-z0-9-]{0,63}$/u;
const DEFAULT_PROFILES = ["headless", "observed", "interactive", "paired-visible", "paired-observed"];

export type WorkspaceTrustEntry = {
  canonicalPath: string;
  approvedAt: string;
  approvedBy: "local-operator";
  device: number;
  inode: number;
  expiresAt?: string;
  allowedProfiles: string[];
};

export type TrustedWorkspaceIdentity = {
  canonicalRoot: string;
  trustRecordDigest: `sha256:${string}`;
  entry: WorkspaceTrustEntry;
};

export class WorkspaceTrustError extends Error {
  readonly code = "WORKSPACE_NOT_TRUSTED" as const;

  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "WorkspaceTrustError";
  }
}

type GitWorkspace = Readonly<{ root: string; linkedWorktree: boolean }>;

function isMissingPathError(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error &&
    (error.code === "ENOENT" || error.code === "ENOTDIR");
}

function hasErrorCode(error: unknown, code: string): boolean {
  return typeof error === "object" && error !== null && "code" in error && error.code === code;
}

// Gitfiles also identify submodules, so the file alone does not prove a linked
// worktree. Only its canonical gitdir under another repository's
// `.git/worktrees/` boundary activates the user-only exception guidance.
function pointsToLinkedWorktree(gitDirectory: string, workspaceRoot: string): boolean {
  let candidate = gitDirectory;
  for (;;) {
    const parent = dirname(candidate);
    if (basename(parent) === "worktrees" && basename(dirname(parent)) === ".git") {
      return dirname(dirname(parent)) !== workspaceRoot;
    }
    if (parent === candidate) return false;
    candidate = parent;
  }
}

export async function nearestGitWorkspace(canonicalRoot: string): Promise<GitWorkspace | null> {
  let candidate = canonicalRoot;
  for (;;) {
    const filesystemRoot = candidate === parse(candidate).root;
    let marker: Awaited<ReturnType<typeof lstat>>;
    try {
      marker = await lstat(join(candidate, ".git"));
    } catch (error: unknown) {
      // macOS may report EINVAL rather than ENOENT for `/.git` in a sandbox.
      // It is a missing root marker only at this terminal probe; the same
      // error anywhere else leaves the boundary indeterminate and fail-closed.
      if (!isMissingPathError(error) && !(filesystemRoot && hasErrorCode(error, "EINVAL"))) throw error;
      if (filesystemRoot) return null;
      candidate = dirname(candidate);
      continue;
    }
    if (marker.isDirectory()) return { root: candidate, linkedWorktree: false };
    if (marker.isFile()) {
      const match = /^gitdir:\s*(?<path>.+?)\s*$/u.exec(await readFile(join(candidate, ".git"), "utf8"));
      const gitDirectory = match?.groups?.path;
      if (gitDirectory === undefined) throw new Error("Git workspace marker is not a gitdir file");
      const canonicalGitDirectory = await realpath(resolve(candidate, gitDirectory));
      return {
        root: candidate,
        linkedWorktree: pointsToLinkedWorktree(canonicalGitDirectory, candidate),
      };
    }
    throw new Error("Git workspace marker must be a directory or regular file");
  }
}

export async function repositoryCollectionChildren(canonicalRoot: string): Promise<string[]> {
  const children = await readdir(canonicalRoot, { withFileTypes: true });
  const repositories = new Set<string>();
  for (const child of children) {
    try {
      const canonicalChild = await realpath(join(canonicalRoot, child.name));
      if (!(await lstat(canonicalChild)).isDirectory()) continue;
      const marker = await lstat(join(canonicalChild, ".git"));
      if (marker.isDirectory() || marker.isFile()) repositories.add(canonicalChild);
    } catch (error: unknown) {
      if (!isMissingPathError(error)) throw error;
    }
  }
  return [...repositories];
}

const WORKSPACE_FILE_MARKERS = new Set([
  "AGENTS.md",
  "CLAUDE.md",
  "package.json",
  "pyproject.toml",
  "Cargo.toml",
  "go.mod",
]);

function workspaceMarkerKind(name: string): "file" | "directory" | null {
  if (name === ".claude") return "directory";
  if (WORKSPACE_FILE_MARKERS.has(name) || name.endsWith(".code-workspace")) return "file";
  return null;
}

function isCanonicalDescendant(canonicalRoot: string, canonicalPath: string): boolean {
  const relativePath = relative(canonicalRoot, canonicalPath);
  return relativePath === "" || (relativePath !== ".." &&
    !relativePath.startsWith(`..${sep}`) && !isAbsolute(relativePath));
}

async function isRepositoryRoot(canonicalPath: string): Promise<boolean> {
  try {
    const marker = await lstat(join(canonicalPath, ".git"));
    return marker.isDirectory() || marker.isFile();
  } catch (error: unknown) {
    if (isMissingPathError(error)) return false;
    throw error;
  }
}

async function hasWorkspaceMarker(canonicalRoot: string): Promise<boolean> {
  const children = await readdir(canonicalRoot, { withFileTypes: true });
  for (const child of children) {
    const kind = workspaceMarkerKind(child.name);
    if (kind === null) continue;
    try {
      // Resolve before inspecting so a project marker symlinked into a child
      // repository is treated the same as a marker stored at this root.
      const resolved = await realpath(join(canonicalRoot, child.name));
      if (resolved === canonicalRoot && child.isSymbolicLink()) continue;
      if (!isCanonicalDescendant(canonicalRoot, resolved)) continue;
      const marker = await lstat(resolved);
      // A directory marker that is itself a repository root is a sibling
      // clone, not a project signal. Anyone able to place a repository here
      // could otherwise name it `.claude` and turn the collection guard off
      // for every other repository beside it.
      if (kind === "directory" && await isRepositoryRoot(resolved)) continue;
      if ((kind === "file" && marker.isFile()) || (kind === "directory" && marker.isDirectory())) return true;
    } catch (error: unknown) {
      if (!isMissingPathError(error)) throw error;
    }
  }
  return false;
}

// The collection heuristic is intentionally shallow: direct repository
// children show that trusting this directory would grant sibling authority,
// while recursively searching would misclassify an ordinary project that
// happens to contain nested fixtures or vendored repositories. A conventional
// marker at this exact root is an explicit project signal, even when the
// project deliberately composes several repositories.
export async function looksLikeRepositoryCollection(canonicalRoot: string): Promise<boolean> {
  if ((await repositoryCollectionChildren(canonicalRoot)).length <= 1) return false;
  return !await hasWorkspaceMarker(canonicalRoot);
}

type WorkspaceTrustRegistry = { schemaVersion: 1; entries: WorkspaceTrustEntry[] };
let mutationQueue: Promise<void> = Promise.resolve();

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}

function timestamp(value: string, field: string): number {
  const parsed = Date.parse(value);
  if (!Number.isFinite(parsed) || new Date(parsed).toISOString() !== value) throw new Error(`${field} must be an ISO timestamp`);
  return parsed;
}

function validateRegistry(value: unknown): WorkspaceTrustRegistry {
  if (typeof value !== "object" || value === null || Array.isArray(value) || !("schemaVersion" in value) || value.schemaVersion !== 1 || !("entries" in value) || !Array.isArray(value.entries)) {
    throw new Error("workspace trust registry is invalid");
  }
  const entries: WorkspaceTrustEntry[] = value.entries.map((candidate) => {
    if (
      typeof candidate !== "object" || candidate === null || Array.isArray(candidate) ||
      !("canonicalPath" in candidate) || typeof candidate.canonicalPath !== "string" || !isAbsolute(candidate.canonicalPath) ||
      !("approvedAt" in candidate) || typeof candidate.approvedAt !== "string" ||
      !("approvedBy" in candidate) || candidate.approvedBy !== "local-operator" ||
      !("device" in candidate) || typeof candidate.device !== "number" || !Number.isSafeInteger(candidate.device) || candidate.device < 0 ||
      !("inode" in candidate) || typeof candidate.inode !== "number" || !Number.isSafeInteger(candidate.inode) || candidate.inode < 0 ||
      !("allowedProfiles" in candidate) || !Array.isArray(candidate.allowedProfiles) || candidate.allowedProfiles.length === 0 ||
      candidate.allowedProfiles.some((profile: unknown) => typeof profile !== "string" || !PROFILE_PATTERN.test(profile)) ||
      ("expiresAt" in candidate && candidate.expiresAt !== undefined && typeof candidate.expiresAt !== "string")
    ) throw new Error("workspace trust entry is invalid");
    timestamp(candidate.approvedAt, "workspace approval");
    if (typeof candidate.expiresAt === "string") timestamp(candidate.expiresAt, "workspace expiry");
    return {
      canonicalPath: candidate.canonicalPath,
      approvedAt: candidate.approvedAt,
      approvedBy: "local-operator",
      device: candidate.device,
      inode: candidate.inode,
      ...(typeof candidate.expiresAt === "string" ? { expiresAt: candidate.expiresAt } : {}),
      allowedProfiles: [...new Set(candidate.allowedProfiles as string[])].sort(),
    };
  });
  if (new Set(entries.map((entry) => entry.canonicalPath)).size !== entries.length) throw new Error("workspace trust entries must be unique");
  return { schemaVersion: 1, entries: entries.sort((left, right) => left.canonicalPath.localeCompare(right.canonicalPath)) };
}

async function readRegistry(path: string): Promise<WorkspaceTrustRegistry> {
  try {
    const before = await lstat(path);
    if (!before.isFile() || before.isSymbolicLink() || (before.mode & 0o077) !== 0) throw new Error("workspace trust registry must be a private regular file");
    const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const opened = await handle.stat();
      if (!opened.isFile() || opened.dev !== before.dev || opened.ino !== before.ino || (opened.mode & 0o077) !== 0) throw new Error("workspace trust registry changed while opening");
      return validateRegistry(JSON.parse(await handle.readFile("utf8")));
    } finally {
      await handle.close();
    }
  } catch (error: unknown) {
    if (errorCode(error) === "ENOENT") return { schemaVersion: 1, entries: [] };
    throw error;
  }
}

async function writeRegistry(path: string, registry: WorkspaceTrustRegistry): Promise<void> {
  try {
    const existing = await lstat(path);
    if (!existing.isFile() || existing.isSymbolicLink() || (existing.mode & 0o077) !== 0) {
      throw new Error("workspace trust registry must be a private regular file");
    }
  } catch (error: unknown) {
    if (errorCode(error) !== "ENOENT") throw error;
  }
  const temporary = `${path}.tmp-${process.pid}-${randomBytes(12).toString("hex")}`;
  const handle = await open(temporary, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | constants.O_NOFOLLOW, 0o600);
  try {
    await handle.writeFile(`${JSON.stringify(validateRegistry(registry), null, 2)}\n`, "utf8");
    await handle.sync();
  } finally {
    await handle.close();
  }
  try {
    await rename(temporary, path);
    // The temporary inode was created privately, so rename is the visibility
    // commit point. These follow-up operations improve durability or repair
    // metadata, but must not turn a visible grant into a reported refusal.
    try { await chmod(path, 0o600); } catch { /* best effort after commit */ }
    try {
      const directory = await open(dirname(path), constants.O_RDONLY);
      try { await directory.sync(); } finally { await directory.close(); }
    } catch { /* directory fsync is best effort after commit */ }
  } finally {
    await rm(temporary, { force: true });
  }
}

async function withRegistryMutationLock<T>(stateDirectory: string, operation: () => Promise<T>): Promise<T> {
  let releaseQueue!: () => void;
  const previous = mutationQueue;
  mutationQueue = new Promise<void>((resolveQueue) => { releaseQueue = resolveQueue; });
  await previous;
  const lockPath = join(stateDirectory, "trusted-workspaces.lock.sqlite3");
  const lock = new Database(lockPath);
  try {
    lock.pragma("busy_timeout = 10000");
    lock.exec("CREATE TABLE IF NOT EXISTS registry_lock(singleton INTEGER PRIMARY KEY CHECK(singleton=1)); BEGIN IMMEDIATE");
    await chmod(lockPath, 0o600);
    try {
      const result = await operation();
      lock.exec("COMMIT");
      return result;
    } catch (error: unknown) {
      if (lock.inTransaction) lock.exec("ROLLBACK");
      throw error;
    }
  } finally {
    lock.close();
    releaseQueue();
  }
}

async function canonicalWorkspace(path: string): Promise<{ canonicalPath: string; device: number; inode: number }> {
  if (path.split(/[\\/]/u).includes("..")) throw new Error("workspace trust refuses lexical ancestor broadening");
  const requested = resolve(path);
  const requestedInfo = await lstat(requested);
  if (requestedInfo.isSymbolicLink()) throw new Error("workspace trust does not accept a symbolic-link root");
  if (!requestedInfo.isDirectory()) throw new Error("trusted workspace must be a directory");
  const canonical = await realpath(requested);
  const canonicalInfo = await lstat(canonical);
  if (!canonicalInfo.isDirectory() || canonicalInfo.isSymbolicLink()) throw new Error("trusted workspace identity is unsafe");
  const home = await realpath(homedir());
  if (canonical === parse(canonical).root) {
    throw new Error(
      `workspace trust refuses ${canonical}: the filesystem root can never be trusted because root-wide authority is forbidden by policy. Run provenant fabric workspace trust /path/to/exact-repository instead`,
    );
  }
  if (canonical === home) {
    throw new Error(
      `workspace trust refuses ${canonical}: this exact path can never be trusted because home-wide authority is forbidden by policy. Run provenant fabric workspace trust /path/to/exact-repository instead`,
    );
  }
  return { canonicalPath: canonical, device: canonicalInfo.dev, inode: canonicalInfo.ino };
}

// `st_dev` is a runtime mount handle the kernel assigns at mount time, not durable
// identity: macOS renumbers APFS volumes across reboots, which silently invalidated
// every record written before the last one. The canonical path, the inode and the
// directory type still pin the same filesystem object, so a device-only change is
// drift to heal rather than evidence of a swapped root. Substituting a different
// directory at the recorded path changes the inode and is still refused; forging a
// device change alone would mean mounting over the path, which needs the privilege
// to rewrite this registry directly.
type WorkspaceIdentityState = "match" | "device-drifted" | "mismatch";

async function identityState(entry: WorkspaceTrustEntry): Promise<WorkspaceIdentityState> {
  try {
    const info = await lstat(entry.canonicalPath);
    if (!info.isDirectory() || info.isSymbolicLink()) return "mismatch";
    if (info.ino !== entry.inode) return "mismatch";
    if (await realpath(entry.canonicalPath) !== entry.canonicalPath) return "mismatch";
    return info.dev === entry.device ? "match" : "device-drifted";
  } catch {
    return "mismatch";
  }
}

async function identityMatches(entry: WorkspaceTrustEntry): Promise<boolean> {
  return await identityState(entry) !== "mismatch";
}

// A removed root cannot be canonicalised, yet the record it must be matched against
// holds a canonical path. Resolving the deepest surviving ancestor and re-attaching
// the rest recovers it; a purely lexical fallback would miss every path under a
// symlinked ancestor, which on macOS includes everything below `/var`.
async function bestEffortCanonicalPath(target: string): Promise<string> {
  const absolute = resolve(target);
  const suffix: string[] = [];
  let probe = absolute;
  for (;;) {
    const resolved = await realpath(probe).catch(() => undefined);
    if (resolved !== undefined) return join(resolved, ...suffix);
    const parent = dirname(probe);
    if (parent === probe) return absolute;
    suffix.unshift(basename(probe));
    probe = parent;
  }
}

function trustRecordDigest(entry: WorkspaceTrustEntry): `sha256:${string}` {
  const normalized = JSON.stringify({
    allowedProfiles: entry.allowedProfiles,
    approvedAt: entry.approvedAt,
    approvedBy: entry.approvedBy,
    canonicalPath: entry.canonicalPath,
    device: entry.device,
    ...(entry.expiresAt === undefined ? {} : { expiresAt: entry.expiresAt }),
    inode: entry.inode,
  });
  return `sha256:${createHash("sha256").update(normalized).digest("hex")}`;
}

async function collectionBoundarySatisfied(canonicalRoot: string): Promise<boolean> {
  try {
    const workspace = await nearestGitWorkspace(canonicalRoot);
    return workspace !== null || !await looksLikeRepositoryCollection(canonicalRoot);
  } catch (error: unknown) {
    if (errorCode(error) === "ENOENT") return false;
    throw error;
  }
}

function collectionBoundaryError(canonicalRoot: string, repositories: string[]): WorkspaceTrustError {
  const shellQuote = (value: string) => `'${value.replaceAll("'", `\'"\'"\'`)}'`;
  const quotedRepositories = repositories.map(shellQuote);
  const commands = repositories.map((repository) => `provenant fabric workspace trust ${shellQuote(repository)}`).join("; ");
  return new WorkspaceTrustError(
    `workspace trust refuses ${shellQuote(canonicalRoot)} because it is a repository collection; trust an exact repository root instead: ${quotedRepositories.join(", ")}. Run ${commands}`,
  );
}

function option(arguments_: string[], name: string): string | undefined {
  const index = arguments_.indexOf(name);
  const value = index === -1 ? undefined : arguments_[index + 1];
  if (index !== -1 && (value === undefined || value.startsWith("--"))) throw new Error(`${name} requires a value`);
  return value;
}

export async function trustedWorkspaceRoots(input: {
  stateDirectory: string;
  executionProfile?: string;
  now?: Date;
}): Promise<string[]> {
  const registry = await readRegistry(join(input.stateDirectory, "trusted-workspaces.json"));
  const now = (input.now ?? new Date()).getTime();
  const candidates = registry.entries
    .filter((entry) => input.executionProfile === undefined || entry.allowedProfiles.includes(input.executionProfile))
    .filter((entry) => entry.expiresAt === undefined || timestamp(entry.expiresAt, "workspace expiry") > now);
  const matches = await Promise.all(candidates.map(async (entry) =>
    await identityMatches(entry) && await collectionBoundarySatisfied(entry.canonicalPath)));
  return candidates.filter((_entry, index) => matches[index] === true).map((entry) => entry.canonicalPath);
}

export async function trustedWorkspaceIdentity(input: {
  stateDirectory: string;
  canonicalRoot: string;
  executionProfile?: string;
  now?: Date;
}): Promise<TrustedWorkspaceIdentity> {
  const identity = await canonicalWorkspace(input.canonicalRoot);
  const registry = await readRegistry(join(input.stateDirectory, "trusted-workspaces.json"));
  const entry = registry.entries.find((candidate) => candidate.canonicalPath === identity.canonicalPath);
  if (entry === undefined) throw new Error("workspace root is not trusted");
  if (entry.expiresAt !== undefined && timestamp(entry.expiresAt, "workspace expiry") <= (input.now ?? new Date()).getTime()) {
    throw new Error("workspace trust record is expired");
  }
  if (input.executionProfile !== undefined && !entry.allowedProfiles.includes(input.executionProfile)) {
    throw new Error("workspace trust record does not allow the requested profile");
  }
  if (!await identityMatches(entry)) throw new Error("workspace trust record no longer matches the live root identity");
  if (!await collectionBoundarySatisfied(identity.canonicalPath)) {
    throw collectionBoundaryError(identity.canonicalPath, await repositoryCollectionChildren(identity.canonicalPath));
  }
  return {
    canonicalRoot: entry.canonicalPath,
    trustRecordDigest: trustRecordDigest(entry),
    entry: { ...entry, allowedProfiles: [...entry.allowedProfiles] },
  };
}

export async function runWorkspaceTrust(
  arguments_: string[],
  paths: FabricPaths,
  now = new Date(),
): Promise<Record<string, unknown>> {
  const requestedAction = arguments_[0];
  const action = requestedAction === "status" ? "inspect" : requestedAction;
  const registryPath = join(paths.stateDirectory, "trusted-workspaces.json");
  const registry = await readRegistry(registryPath);
  if (action === "list") {
    // A record whose root has been removed or replaced is dead, but rendering it
    // exactly like a live one made `list` say trusted where `inspect` said false.
    const states = await Promise.all(registry.entries.map(identityState));
    const boundaries = await Promise.all(registry.entries.map((entry) => collectionBoundarySatisfied(entry.canonicalPath)));
    return {
      schemaVersion: 1,
      registryPath,
      entries: registry.entries.map((entry, index) => ({
        ...entry,
        identity: states[index],
        expired: entry.expiresAt !== undefined && timestamp(entry.expiresAt, "workspace expiry") <= now.getTime(),
        trusted: boundaries[index] && states[index] !== "mismatch" &&
          (entry.expiresAt === undefined || timestamp(entry.expiresAt, "workspace expiry") > now.getTime()),
      })),
    };
  }
  const requested = arguments_[1];
  if (requested === undefined || requested.startsWith("--")) {
    throw new Error(`workspace ${String(requestedAction)} requires a path`);
  }
  if (action === "revoke") {
    // Revoking only ever narrows authority, so it must not require the directory to
    // still exist. Canonicalising first made a removed worktree unrevokable, and a
    // stale descendant record blocks an exact-root re-trust, so the pair deadlocked
    // recovery: the only remaining escape was hand-editing the registry.
    const resolvedPath = resolve(requested);
    const canonicalCandidate = await bestEffortCanonicalPath(resolvedPath);
    ensureFabricPaths(paths);
    return await withRegistryMutationLock(paths.stateDirectory, async () => {
      const current = await readRegistry(registryPath);
      const doomed = current.entries.filter(
        (entry) => entry.canonicalPath === canonicalCandidate || entry.canonicalPath === resolvedPath,
      );
      const [first] = doomed;
      if (first === undefined) return { schemaVersion: 1, canonicalPath: canonicalCandidate, revoked: false };
      await writeRegistry(registryPath, {
        schemaVersion: 1,
        entries: current.entries.filter((entry) => !doomed.includes(entry)),
      });
      return { schemaVersion: 1, canonicalPath: first.canonicalPath, revoked: true };
    });
  }
  const identity = await canonicalWorkspace(requested);
  const { canonicalPath } = identity;
  const existing = registry.entries.find((entry) => entry.canonicalPath === canonicalPath);
  if (action === "inspect") {
    const expired = existing?.expiresAt !== undefined && timestamp(existing.expiresAt, "workspace expiry") <= now.getTime();
    const trusted = existing !== undefined && !expired && await identityMatches(existing) &&
      await collectionBoundarySatisfied(canonicalPath);
    return { schemaVersion: 1, canonicalPath, trusted, expired, entry: existing ?? null };
  }
  if (action !== "trust") throw new Error("workspace command must be trust, inspect, status, list or revoke");
  ensureFabricPaths(paths);
  const profileValue = option(arguments_, "--profiles");
  const requestedProfiles = profileValue?.split(",")
    .map((profile) => profile.trim())
    .filter((profile) => profile.length > 0);
  if (requestedProfiles !== undefined &&
    (requestedProfiles.length === 0 || requestedProfiles.some((profile) => !PROFILE_PATTERN.test(profile)))) {
    throw new Error("workspace profiles are invalid");
  }
  const expiresAt = option(arguments_, "--expires-at");
  if (expiresAt !== undefined && timestamp(expiresAt, "workspace expiry") <= now.getTime()) throw new Error("workspace trust expiry must be in the future");
  return await withRegistryMutationLock(paths.stateDirectory, async () => {
    const current = await readRegistry(registryPath);
    const currentEntry = current.entries.find((item) => item.canonicalPath === canonicalPath);
    const currentEntryState = currentEntry === undefined ? "mismatch" : await identityState(currentEntry);
    const currentEntryIdentityMatches = currentEntryState !== "mismatch";
    const currentEntryIsLive = currentEntry !== undefined &&
      (currentEntry.expiresAt === undefined || timestamp(currentEntry.expiresAt, "workspace expiry") > now.getTime()) &&
      currentEntryIdentityMatches;
    if (currentEntry === undefined || !currentEntryIdentityMatches) {
      const broadened = current.entries.find((item) => item.canonicalPath.startsWith(`${canonicalPath}${sep}`));
      if (broadened !== undefined) throw new Error(`workspace trust refuses ancestor broadening over ${broadened.canonicalPath}`);
    }
    // A drifted record is honoured, but re-trusting it is the moment to write the
    // live device back, so the stale number does not persist forever.
    const alreadyTrusted = currentEntryIsLive && currentEntryState === "match" &&
      profileValue === undefined && expiresAt === undefined;
    const allowedProfiles = requestedProfiles ?? currentEntry?.allowedProfiles ?? DEFAULT_PROFILES;
    const currentExpiryIsLive = currentEntry?.expiresAt !== undefined &&
      timestamp(currentEntry.expiresAt, "workspace expiry") > now.getTime();
    const effectiveExpiry = expiresAt ?? (currentExpiryIsLive ? currentEntry.expiresAt : undefined);
    if (effectiveExpiry !== undefined && timestamp(effectiveExpiry, "workspace expiry") <= now.getTime()) {
      throw new Error("workspace trust expiry must be in the future");
    }
    const entry: WorkspaceTrustEntry = {
      canonicalPath,
      approvedAt: now.toISOString(),
      approvedBy: "local-operator",
      device: identity.device,
      inode: identity.inode,
      ...(effectiveExpiry === undefined ? {} : { expiresAt: effectiveExpiry }),
      allowedProfiles: [...new Set(allowedProfiles)].sort(),
    };
    if (!await collectionBoundarySatisfied(canonicalPath)) {
      throw collectionBoundaryError(canonicalPath, await repositoryCollectionChildren(canonicalPath));
    }
    if (alreadyTrusted && currentEntry !== undefined) {
      return {
        schemaVersion: 1,
        trusted: true,
        alreadyTrusted: true,
        entry: { ...currentEntry, allowedProfiles: [...currentEntry.allowedProfiles] },
      };
    }
    await writeRegistry(registryPath, {
      schemaVersion: 1,
      entries: [...current.entries.filter((item) => item.canonicalPath !== canonicalPath), entry],
    });
    return { schemaVersion: 1, trusted: true, entry };
  });
}
