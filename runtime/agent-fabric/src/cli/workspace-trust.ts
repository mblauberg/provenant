import { createHash, randomBytes } from "node:crypto";
import Database from "better-sqlite3";
import { constants } from "node:fs";
import { chmod, lstat, mkdir, open, realpath, rename, rm } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, parse, resolve, sep } from "node:path";

import { ensureFabricPaths, type FabricPaths } from "./paths.js";
import {
  looksLikeRepositoryCollection,
  projectBoundaryEvidenceDigest,
  repositoryCollectionChildren,
  resolveProjectBoundary,
  type ProjectBoundary,
} from "./project-boundary.js";

export {
  looksLikeRepositoryCollection,
  nearestGitWorkspace,
  repositoryCollectionChildren,
} from "./project-boundary.js";

const PROFILE_PATTERN = /^[a-z][a-z0-9-]{0,63}$/u;
const DEFAULT_PROFILES = ["headless", "observed", "interactive", "paired-visible", "paired-observed"];
const TRUST_RECORD_DIGEST_PATTERN = /^sha256:[0-9a-f]{64}$/u;

type WorkspaceTrustEntryWithoutDigest = {
  canonicalPath: string;
  approvedAt: string;
  device: number;
  inode: number;
  expiresAt?: string;
  allowedProfiles: string[];
  establishmentKind: "automatic-bootstrap" | "local-operator";
  /** Present only on a grant made by the first-use bootstrap saga. */
  boundaryKind?: "git" | "project-marker" | "non-git";
  boundaryEvidenceDigest?: `sha256:${string}`;
  bootstrapAttemptId?: string;
};

export type WorkspaceTrustEntry = WorkspaceTrustEntryWithoutDigest & {
  /** The exact identity bound into project custody; it is not always a v2 byte digest. */
  trustRecordDigest: `sha256:${string}`;
  /** Names the digest projection that produced trustRecordDigest. */
  trustRecordDigestVersion: "legacy-v1" | "canonical-v2";
};

export type AutomaticBootstrapTrustResult = Readonly<{
  identity: TrustedWorkspaceIdentity;
  mutated: boolean;
  alreadyTrusted: boolean;
  /** The attempt that made or observed this request. */
  requestAttemptId: string;
  /** The persisted automatic grant attempt, or null for an explicit grant. */
  bootstrapAttemptId: string | null;
  boundaryKind: "git" | "project-marker" | "non-git" | null;
  boundaryEvidenceDigest: `sha256:${string}` | null;
}>;

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

export type WorkspaceTrustFailureContext = Readonly<{
  boundary: ProjectBoundary;
  boundaryEvidenceDigest: `sha256:${string}`;
}>;

const workspaceTrustFailureContexts = new WeakMap<WorkspaceTrustError, WorkspaceTrustFailureContext>();

function boundaryTrustError(
  boundary: ProjectBoundary,
  message: string,
  options?: ErrorOptions,
): WorkspaceTrustError {
  const error = new WorkspaceTrustError(message, options);
  workspaceTrustFailureContexts.set(error, {
    boundary,
    boundaryEvidenceDigest: projectBoundaryEvidenceDigest(boundary),
  });
  return error;
}

export function workspaceTrustFailureContext(error: unknown): WorkspaceTrustFailureContext | undefined {
  return error instanceof WorkspaceTrustError ? workspaceTrustFailureContexts.get(error) : undefined;
}

const LEGACY_WORKSPACE_TRUST_SCHEMA_VERSION = 1;
const WORKSPACE_TRUST_SCHEMA_VERSION = 2;
// Version 1 is accepted only while this process performs the one-way local
// cutover. Persisted canonical records are version 2, so an old reader fails
// closed instead of treating a field-removed record as version 1.
type WorkspaceTrustRegistry = { schemaVersion: typeof WORKSPACE_TRUST_SCHEMA_VERSION; entries: WorkspaceTrustEntry[] };
type ParsedWorkspaceTrustRegistry = Readonly<{
  registry: WorkspaceTrustRegistry;
  requiresCanonicalization: boolean;
}>;
let mutationQueue: Promise<void> = Promise.resolve();

function errorCode(error: unknown): string | undefined {
  return typeof error === "object" && error !== null && "code" in error && typeof error.code === "string"
    ? error.code
    : undefined;
}

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function shellQuote(value: string): string {
  return `'${value.replaceAll("'", `\\'"'"'`)}'`;
}

function timestamp(value: string, field: string): number {
  const parsed = Date.parse(value);
  if (!Number.isFinite(parsed) || new Date(parsed).toISOString() !== value) throw new Error(`${field} must be an ISO timestamp`);
  return parsed;
}

function parseRegistry(value: unknown): ParsedWorkspaceTrustRegistry {
  if (typeof value !== "object" || value === null || Array.isArray(value) || !("schemaVersion" in value) ||
    (value.schemaVersion !== LEGACY_WORKSPACE_TRUST_SCHEMA_VERSION && value.schemaVersion !== WORKSPACE_TRUST_SCHEMA_VERSION) ||
    !("entries" in value) || !Array.isArray(value.entries)) {
    throw new Error("workspace trust registry is invalid");
  }
  const legacySchema = value.schemaVersion === LEGACY_WORKSPACE_TRUST_SCHEMA_VERSION;
  let requiresCanonicalization = legacySchema;
  const entries: WorkspaceTrustEntry[] = value.entries.map((candidate) => {
    if (
      typeof candidate !== "object" || candidate === null || Array.isArray(candidate) ||
      !("canonicalPath" in candidate) || typeof candidate.canonicalPath !== "string" || !isAbsolute(candidate.canonicalPath) ||
      !("approvedAt" in candidate) || typeof candidate.approvedAt !== "string" ||
      !("device" in candidate) || typeof candidate.device !== "number" || !Number.isSafeInteger(candidate.device) || candidate.device < 0 ||
      !("inode" in candidate) || typeof candidate.inode !== "number" || !Number.isSafeInteger(candidate.inode) || candidate.inode < 0 ||
      !("allowedProfiles" in candidate) || !Array.isArray(candidate.allowedProfiles) || candidate.allowedProfiles.length === 0 ||
      candidate.allowedProfiles.some((profile: unknown) => typeof profile !== "string" || !PROFILE_PATTERN.test(profile)) ||
      ("expiresAt" in candidate && candidate.expiresAt !== undefined && typeof candidate.expiresAt !== "string")
    ) throw new Error("workspace trust entry is invalid");
    const record = candidate as Record<string, unknown>;
    const establishmentKind = record.establishmentKind;
    const approvedBy = record.approvedBy;
    const hasLegacyApproval = "approvedBy" in record;
    if (!legacySchema && hasLegacyApproval) throw new Error("workspace trust registry contains legacy approval provenance");
    if (legacySchema && !hasLegacyApproval) throw new Error("workspace trust entry approval provenance is invalid");
    if (legacySchema && ("trustRecordDigest" in record || "trustRecordDigestVersion" in record)) {
      throw new Error("legacy workspace trust registry contains digest provenance");
    }
    if (hasLegacyApproval && approvedBy !== "local-operator") throw new Error("workspace trust entry approval provenance is invalid");
    if (legacySchema && establishmentKind === "local-operator") {
      throw new Error("workspace trust entry establishment kind is invalid");
    }
    if (establishmentKind !== undefined && establishmentKind !== "automatic-bootstrap" && establishmentKind !== "local-operator") {
      throw new Error("workspace trust entry establishment kind is invalid");
    }
    const canonicalEstablishmentKind = establishmentKind === undefined
      ? (hasLegacyApproval ? "local-operator" : undefined)
      : establishmentKind;
    if (canonicalEstablishmentKind === undefined) throw new Error("workspace trust entry provenance is unknown");
    if (hasLegacyApproval) requiresCanonicalization = true;
    const boundaryKind = record.boundaryKind;
    const boundaryEvidenceDigest = record.boundaryEvidenceDigest;
    const bootstrapAttemptId = record.bootstrapAttemptId;
    if (canonicalEstablishmentKind === "automatic-bootstrap") {
      if (
        (boundaryKind !== "git" && boundaryKind !== "project-marker" && boundaryKind !== "non-git") ||
        typeof boundaryEvidenceDigest !== "string" || !/^sha256:[0-9a-f]{64}$/u.test(boundaryEvidenceDigest) ||
        typeof bootstrapAttemptId !== "string" || bootstrapAttemptId.length === 0 ||
        JSON.stringify(candidate.allowedProfiles) !== JSON.stringify(["headless"])
      ) throw new Error("automatic workspace trust provenance is invalid");
    } else if ("boundaryKind" in record || "boundaryEvidenceDigest" in record || "bootstrapAttemptId" in record) {
      throw new Error("workspace trust provenance is incomplete");
    }
    timestamp(candidate.approvedAt, "workspace approval");
    if (typeof candidate.expiresAt === "string") timestamp(candidate.expiresAt, "workspace expiry");
    const entryWithoutDigest: WorkspaceTrustEntryWithoutDigest = {
      canonicalPath: candidate.canonicalPath,
      approvedAt: candidate.approvedAt,
      device: candidate.device,
      inode: candidate.inode,
      ...(typeof candidate.expiresAt === "string" ? { expiresAt: candidate.expiresAt } : {}),
      allowedProfiles: [...new Set(candidate.allowedProfiles as string[])].sort(),
      establishmentKind: canonicalEstablishmentKind,
      ...(canonicalEstablishmentKind === "automatic-bootstrap" ? {
        boundaryKind: boundaryKind as "git" | "project-marker" | "non-git",
        boundaryEvidenceDigest: boundaryEvidenceDigest as `sha256:${string}`,
        bootstrapAttemptId: bootstrapAttemptId as string,
      } : {}),
    };
    if (legacySchema) {
      return {
        ...entryWithoutDigest,
        trustRecordDigest: legacyTrustRecordDigest(entryWithoutDigest),
        trustRecordDigestVersion: "legacy-v1",
      };
    }
    const digestVersion = record.trustRecordDigestVersion;
    if (digestVersion !== "legacy-v1" && digestVersion !== "canonical-v2") {
      throw new Error("workspace trust digest provenance is invalid");
    }
    const expectedDigest = digestVersion === "legacy-v1"
      ? legacyTrustRecordDigest(entryWithoutDigest)
      : canonicalTrustRecordDigest(entryWithoutDigest);
    if (typeof record.trustRecordDigest !== "string" || !TRUST_RECORD_DIGEST_PATTERN.test(record.trustRecordDigest) ||
      record.trustRecordDigest !== expectedDigest) {
      throw new Error("workspace trust digest provenance is invalid");
    }
    return {
      ...entryWithoutDigest,
      trustRecordDigest: record.trustRecordDigest as `sha256:${string}`,
      trustRecordDigestVersion: digestVersion,
    };
  });
  if (new Set(entries.map((entry) => entry.canonicalPath)).size !== entries.length) throw new Error("workspace trust entries must be unique");
  return {
    registry: { schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION, entries: entries.sort((left, right) => left.canonicalPath.localeCompare(right.canonicalPath)) },
    requiresCanonicalization,
  };
}

function validateRegistry(value: unknown): WorkspaceTrustRegistry {
  return parseRegistry(value).registry;
}

async function readRegistry(path: string): Promise<ParsedWorkspaceTrustRegistry> {
  try {
    const before = await lstat(path);
    if (!before.isFile() || before.isSymbolicLink() || (before.mode & 0o077) !== 0) throw new Error("workspace trust registry must be a private regular file");
    const handle = await open(path, constants.O_RDONLY | constants.O_NOFOLLOW);
    try {
      const opened = await handle.stat();
      if (!opened.isFile() || opened.dev !== before.dev || opened.ino !== before.ino || (opened.mode & 0o077) !== 0) throw new Error("workspace trust registry changed while opening");
      return parseRegistry(JSON.parse(await handle.readFile("utf8")));
    } finally {
      await handle.close();
    }
  } catch (error: unknown) {
    if (errorCode(error) === "ENOENT") return {
      registry: { schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION, entries: [] },
      requiresCanonicalization: false,
    };
    throw error;
  }
}

type AutomaticBootstrapTrustTestOnly = Readonly<{
  beforeRegistryRename?: () => Promise<void>;
  afterRegistryRename?: () => Promise<void>;
}>;

async function writeRegistry(
  path: string,
  registry: WorkspaceTrustRegistry,
  testOnly?: AutomaticBootstrapTrustTestOnly,
): Promise<void> {
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
    if (process.env.NODE_ENV === "test") {
      await testOnly?.beforeRegistryRename?.();
    }
    await rename(temporary, path);
    if (process.env.NODE_ENV === "test") {
      await testOnly?.afterRegistryRename?.();
    }
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

function canonicalTrustRecordDigest(entry: WorkspaceTrustEntryWithoutDigest): `sha256:${string}` {
  const normalized = JSON.stringify({
    allowedProfiles: entry.allowedProfiles,
    approvedAt: entry.approvedAt,
    canonicalPath: entry.canonicalPath,
    device: entry.device,
    ...(entry.expiresAt === undefined ? {} : { expiresAt: entry.expiresAt }),
    inode: entry.inode,
    establishmentKind: entry.establishmentKind,
    ...(entry.establishmentKind === "automatic-bootstrap" ? {
      boundaryKind: entry.boundaryKind,
      boundaryEvidenceDigest: entry.boundaryEvidenceDigest,
      bootstrapAttemptId: entry.bootstrapAttemptId,
    } : {}),
  });
  return `sha256:${createHash("sha256").update(normalized).digest("hex")}`;
}

function legacyTrustRecordDigest(entry: WorkspaceTrustEntryWithoutDigest): `sha256:${string}` {
  const normalized = JSON.stringify({
    allowedProfiles: entry.allowedProfiles,
    approvedAt: entry.approvedAt,
    approvedBy: "local-operator",
    canonicalPath: entry.canonicalPath,
    device: entry.device,
    ...(entry.expiresAt === undefined ? {} : { expiresAt: entry.expiresAt }),
    inode: entry.inode,
    ...(entry.establishmentKind === "automatic-bootstrap" ? {
      establishmentKind: entry.establishmentKind,
      boundaryKind: entry.boundaryKind,
      boundaryEvidenceDigest: entry.boundaryEvidenceDigest,
      bootstrapAttemptId: entry.bootstrapAttemptId,
    } : {}),
  });
  return `sha256:${createHash("sha256").update(normalized).digest("hex")}`;
}

function canonicalTrustEntry(entry: WorkspaceTrustEntryWithoutDigest): WorkspaceTrustEntry {
  return {
    ...entry,
    trustRecordDigest: canonicalTrustRecordDigest(entry),
    trustRecordDigestVersion: "canonical-v2",
  };
}

async function collectionBoundarySatisfied(canonicalRoot: string): Promise<boolean> {
  try {
    const boundary = await resolveProjectBoundary(canonicalRoot, { selection: "exact" });
    if (boundary.evidence.kind === "refused") return false;
    if (boundary.gitProbe === "unavailable" && boundary.evidence.kind !== "project-marker") return false;
    if (boundary.evidence.kind !== "ambiguous" || boundary.evidence.reason !== "repository-collection") return true;
    return !await looksLikeRepositoryCollection(canonicalRoot);
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

async function collectionChildGuidance(root: string): Promise<{ activations: string[]; repairs: string[] }> {
  const children = await repositoryCollectionChildren(root);
  const activations: string[] = [];
  const repairs: string[] = [];
  for (const child of children) {
    try {
      const boundary = await resolveProjectBoundary(child, { selection: "exact" });
      const actionable = automaticBoundaryKind(boundary) !== null &&
        boundary.selectedProjectRoot === child;
      if (actionable) {
        activations.push(`provenant project activate ${shellQuote(child)}`);
      } else {
        repairs.push(`Inspect and repair ${shellQuote(child)}: linked or invalid boundary evidence`);
      }
    } catch (cause: unknown) {
      repairs.push(`Inspect and repair ${shellQuote(child)}: ${errorDetail(cause)}`);
    }
  }
  return { activations, repairs };
}

async function automaticBoundaryRefusal(boundary: ProjectBoundary, cause?: unknown): Promise<WorkspaceTrustError> {
  const evidence = boundary.evidence;
  const root = boundary.selectedProjectRoot;
  if (evidence.kind === "ambiguous" && evidence.reason === "repository-collection") {
    const guidance = await collectionChildGuidance(root);
    const childSummary = evidence.repositories.map((repository) => shellQuote(repository)).join(", ");
    const activationText = guidance.activations.length > 0
      ? ` Run ${guidance.activations.join("; ")}.`
      : "";
    const repairText = guidance.repairs.length > 0
      ? ` ${guidance.repairs.join(". ")}.`
      : "";
    return boundaryTrustError(
      boundary,
      `automatic bootstrap enrolment refused for ${shellQuote(root)}: the selected directory is a repository collection. Choose one specific valid child repository: ${childSummary}.${activationText}${repairText} No automatic trust was added.`,
      cause === undefined ? undefined : { cause },
    );
  }
  if (boundary.gitProbe === "unavailable") {
    const probeDetail = boundary.gitProbeError === null ? "the Git repository probe was unavailable" :
      `the Git repository probe was unavailable (${boundary.gitProbeError})`;
    return boundaryTrustError(
      boundary,
      `automatic bootstrap enrolment refused for ${shellQuote(root)}: ${probeDetail}; ` +
      "non-Git admission requires a proven not-repository result. Inspect and repair the boundary evidence, then retry from the exact repository root or exact marked non-Git project directory; no automatic trust was added.",
      cause === undefined ? undefined : { cause },
    );
  }
  if (evidence.kind === "refused" && (evidence.reason === "filesystem-root" || evidence.reason === "home")) {
    return boundaryTrustError(
      boundary,
      `automatic bootstrap enrolment refused for ${shellQuote(root)}: ${evidence.detail}. Choose a specific child repository or exact non-Git project directory and retry; no automatic trust was added.`,
      cause === undefined ? undefined : { cause },
    );
  }
  if (evidence.kind === "refused") {
    return boundaryTrustError(
      boundary,
      `automatic bootstrap enrolment refused for ${shellQuote(root)}: ${evidence.detail}. Inspect and repair the boundary evidence, then retry from the exact repository root or exact marked non-Git project directory; no automatic trust was added.`,
      cause === undefined ? undefined : { cause },
    );
  }
  if (evidence.kind === "git" && evidence.linkedWorktree) {
    return boundaryTrustError(
      boundary,
      `automatic bootstrap enrolment refused for ${shellQuote(root)}: the selected directory is a linked worktree; a worktree-path trust exception is a user-only decision. Inspect the worktree and make an explicit exact-root decision; no automatic trust was added.`,
      cause === undefined ? undefined : { cause },
    );
  }
  const detail = evidence.kind === "ambiguous"
    ? "the selected directory is an unmarked non-Git directory"
    : "the boundary evidence is not eligible for automatic bootstrap enrolment";
  const causeDetail = cause === undefined ? "" : ` (${errorDetail(cause)})`;
  return boundaryTrustError(
    boundary,
    `automatic bootstrap enrolment refused for ${shellQuote(root)}: ${detail}${causeDetail}. Run provenant project activate ${shellQuote(root)} for an explicit exact-root decision; no automatic trust was added.`,
    cause === undefined ? undefined : { cause },
  );
}

function automaticBoundaryKind(boundary: ProjectBoundary): "git" | "project-marker" | "non-git" | null {
  if (boundary.evidence.kind === "git" && !boundary.evidence.linkedWorktree &&
    boundary.selectedProjectRoot === boundary.evidence.root) return "git";
  if (boundary.evidence.kind === "project-marker" &&
    boundary.gitProbe === "not-repository" &&
    boundary.selectedProjectRoot === boundary.evidence.root &&
    boundary.selectedProjectRoot === boundary.requestedDirectory) return "project-marker";
  if (boundary.evidence.kind === "ambiguous" &&
    boundary.evidence.reason === "unmarked-non-git" &&
    boundary.gitProbe === "not-repository" &&
    boundary.selectedProjectRoot === boundary.requestedDirectory &&
    boundary.evidence.root === boundary.requestedDirectory) return "non-git";
  return null;
}

function automaticBoundaryMatchesEntry(
  boundary: ProjectBoundary,
  entry: WorkspaceTrustEntry,
): boolean {
  return entry.establishmentKind === "automatic-bootstrap" &&
    entry.boundaryKind !== undefined &&
    entry.boundaryEvidenceDigest !== undefined &&
    boundary.selectedProjectRoot === entry.canonicalPath &&
    automaticBoundaryKind(boundary) === entry.boundaryKind &&
    projectBoundaryEvidenceDigest(boundary) === entry.boundaryEvidenceDigest;
}

async function validateAutomaticEntryBoundary(entry: WorkspaceTrustEntry): Promise<void> {
  if (
    entry.establishmentKind !== "automatic-bootstrap" ||
    entry.boundaryKind === undefined ||
    entry.boundaryEvidenceDigest === undefined ||
    entry.bootstrapAttemptId === undefined ||
    JSON.stringify(entry.allowedProfiles) !== JSON.stringify(["headless"])
  ) {
    throw new WorkspaceTrustError(
      `automatic workspace trust at ${shellQuote(entry.canonicalPath)} has incomplete provenance; ` +
      "inspect and repair the trust record before retrying",
    );
  }
  let boundary: ProjectBoundary;
  try {
    boundary = await resolveProjectBoundary(entry.canonicalPath, { selection: "exact" });
  } catch (cause: unknown) {
    throw new WorkspaceTrustError(
      `automatic workspace trust at ${shellQuote(entry.canonicalPath)} could not revalidate its live boundary: ${errorDetail(cause)}; ` +
      "inspect and repair the boundary evidence before retrying",
      { cause },
    );
  }
  if (!automaticBoundaryMatchesEntry(boundary, entry)) {
    throw new WorkspaceTrustError(
      `automatic workspace trust at ${shellQuote(entry.canonicalPath)} no longer matches the live boundary evidence; ` +
      "inspect and repair the boundary evidence before retrying",
    );
  }
}

async function automaticEntryIsLive(entry: WorkspaceTrustEntry): Promise<boolean> {
  if (await identityState(entry) === "mismatch") return false;
  if (entry.establishmentKind === "automatic-bootstrap") {
    try {
      await validateAutomaticEntryBoundary(entry);
    } catch {
      return false;
    }
  }
  return await collectionBoundarySatisfied(entry.canonicalPath);
}

function automaticTrustIdentity(entry: WorkspaceTrustEntry): TrustedWorkspaceIdentity {
  return {
    canonicalRoot: entry.canonicalPath,
    trustRecordDigest: entry.trustRecordDigest,
    entry: { ...entry, allowedProfiles: [...entry.allowedProfiles] },
  };
}

/**
 * Establish exact-root trust as part of first-use bootstrap. This is the only
 * automatic writer: it reuses the existing registry lock and writer and never
 * changes an existing grant, including one created by an explicit operator.
 */
export async function ensureAutomaticBootstrapTrust(input: {
  stateDirectory: string;
  bootstrapAttemptId: string;
  cwd: string;
  now?: Date;
  testOnly?: AutomaticBootstrapTrustTestOnly;
}): Promise<AutomaticBootstrapTrustResult> {
  const boundary = await resolveProjectBoundary(input.cwd);
  return ensureAutomaticBootstrapTrustAtBoundary({
    stateDirectory: input.stateDirectory,
    boundary,
    requestAttemptId: input.bootstrapAttemptId,
    ...(input.now === undefined ? {} : { now: input.now }),
    ...(input.testOnly === undefined ? {} : { testOnly: input.testOnly }),
  });
}

async function ensureAutomaticBootstrapTrustAtBoundary(input: {
  stateDirectory: string;
  boundary: ProjectBoundary;
  requestAttemptId: string;
  now?: Date;
  testOnly?: AutomaticBootstrapTrustTestOnly;
}): Promise<AutomaticBootstrapTrustResult> {
  const initialBoundaryKind = automaticBoundaryKind(input.boundary);
  const initialSelectedIdentity = await canonicalWorkspace(input.boundary.selectedProjectRoot).catch(async (cause: unknown) => {
    throw await automaticBoundaryRefusal(input.boundary, cause);
  });
  const initialBoundaryEvidenceDigest = projectBoundaryEvidenceDigest(input.boundary);
  const now = input.now ?? new Date();
  const registryPath = join(input.stateDirectory, "trusted-workspaces.json");
  if (initialBoundaryKind === null) {
    const stateDirectoryExists = await lstat(input.stateDirectory).then((value) => value.isDirectory() && !value.isSymbolicLink()).catch((cause: unknown) => {
      if (errorCode(cause) === "ENOENT") return false;
      throw cause;
    });
    if (!stateDirectoryExists) throw await automaticBoundaryRefusal(input.boundary);
    const existing = (await readRegistry(registryPath)).registry;
    if (!existing.entries.some((entry) => entry.canonicalPath === initialSelectedIdentity.canonicalPath)) {
      throw await automaticBoundaryRefusal(input.boundary);
    }
  }
  await mkdir(input.stateDirectory, { recursive: true, mode: 0o700 });

  return await withRegistryMutationLock(input.stateDirectory, async () => {
    const parsedCurrent = await readRegistry(registryPath);
    const current = parsedCurrent.registry;
    // The first resolution is only a preflight. The registry lock must cover a
    // fresh boundary decision, identity lookup and the grant write so removing
    // or replacing `.git` while this request waits cannot leave stale evidence
    // behind as an automatic grant.
    const liveBoundary = await resolveProjectBoundary(input.boundary.requestedDirectory);
    const liveBoundaryKind = automaticBoundaryKind(liveBoundary);
    const liveBoundaryEvidenceDigest = projectBoundaryEvidenceDigest(liveBoundary);
    const liveSelectedIdentity = await canonicalWorkspace(liveBoundary.selectedProjectRoot).catch(async (cause: unknown) => {
      throw await automaticBoundaryRefusal(liveBoundary, cause);
    });
    let persistedBoundary: ProjectBoundary;
    try {
      // Automatic entries are keyed by the exact selected root. Persist the
      // exact-root snapshot so later liveness checks hash the same boundary
      // shape even when admission began from a nested CWD.
      persistedBoundary = await resolveProjectBoundary(liveSelectedIdentity.canonicalPath, { selection: "exact" });
    } catch (cause: unknown) {
      throw await automaticBoundaryRefusal(liveBoundary, cause);
    }
    const persistedBoundaryKind = automaticBoundaryKind(persistedBoundary);
    const persistedBoundaryEvidenceDigest = projectBoundaryEvidenceDigest(persistedBoundary);
    if (
      liveBoundary.selectedProjectRoot !== input.boundary.selectedProjectRoot ||
      liveBoundaryKind !== initialBoundaryKind ||
      liveBoundaryEvidenceDigest !== initialBoundaryEvidenceDigest ||
      persistedBoundary.selectedProjectRoot !== liveSelectedIdentity.canonicalPath ||
      persistedBoundaryKind !== liveBoundaryKind
    ) {
      throw await automaticBoundaryRefusal(liveBoundary, "the boundary changed while the registry lock was being acquired");
    }
    const currentEntry = current.entries.find((entry) => entry.canonicalPath === liveSelectedIdentity.canonicalPath);
    if (currentEntry !== undefined) {
      const state = await identityState(currentEntry);
      if (state === "mismatch") {
        throw boundaryTrustError(liveBoundary,
          `automatic bootstrap enrolment refused for ${shellQuote(liveSelectedIdentity.canonicalPath)}: ` +
          "the existing trust record no longer matches the live directory identity. " +
          "Inspect and repair the trust record before retrying; no automatic trust was added.",
        );
      }
      if (currentEntry.expiresAt !== undefined && timestamp(currentEntry.expiresAt, "workspace expiry") <= now.getTime()) {
        throw boundaryTrustError(liveBoundary,
          `automatic bootstrap enrolment refused for ${shellQuote(liveSelectedIdentity.canonicalPath)}: ` +
          "the existing trust record is expired. " +
          `Run provenant project activate ${shellQuote(liveSelectedIdentity.canonicalPath)} for an explicit exact-root decision; no automatic trust was added.`,
        );
      }
      if (currentEntry.establishmentKind === "automatic-bootstrap" && !currentEntry.allowedProfiles.includes("headless")) {
        throw boundaryTrustError(liveBoundary,
          `automatic bootstrap enrolment refused for ${shellQuote(liveSelectedIdentity.canonicalPath)}: ` +
          "the existing trust record does not allow the headless profile. " +
          "Inspect and repair the trust record before retrying; no automatic trust was added.",
        );
      }
      if (currentEntry.establishmentKind === "automatic-bootstrap" && !automaticBoundaryMatchesEntry(persistedBoundary, currentEntry)) {
        throw boundaryTrustError(liveBoundary,
          `automatic bootstrap enrolment refused for ${shellQuote(liveSelectedIdentity.canonicalPath)}: ` +
          "the existing automatic trust record has different or stale boundary evidence. " +
          "Inspect and repair the trust record and live boundary before retrying; no automatic trust was added.",
        );
      }
      if (liveBoundary.evidence.kind === "ambiguous" && liveBoundary.evidence.reason === "repository-collection") {
        throw await automaticBoundaryRefusal(liveBoundary);
      }
      if (parsedCurrent.requiresCanonicalization) await writeRegistry(registryPath, current, input.testOnly);
      return {
        identity: automaticTrustIdentity(currentEntry),
        mutated: parsedCurrent.requiresCanonicalization,
        alreadyTrusted: true,
        requestAttemptId: input.requestAttemptId,
        bootstrapAttemptId: currentEntry.establishmentKind === "automatic-bootstrap"
          ? currentEntry.bootstrapAttemptId ?? null
          : null,
        boundaryKind: currentEntry.boundaryKind ?? liveBoundaryKind,
        boundaryEvidenceDigest: currentEntry.boundaryEvidenceDigest ??
          (currentEntry.establishmentKind === "automatic-bootstrap" ? persistedBoundaryEvidenceDigest : null),
      };
    }

    if (liveBoundaryKind === null) throw await automaticBoundaryRefusal(liveBoundary);
    const broadened = current.entries.find((entry) => entry.canonicalPath.startsWith(`${liveSelectedIdentity.canonicalPath}${sep}`));
    if (broadened !== undefined) {
      throw boundaryTrustError(liveBoundary,
        `automatic bootstrap enrolment refused for ${shellQuote(liveSelectedIdentity.canonicalPath)}: ` +
        `it would broaden trust over the existing exact root ${shellQuote(broadened.canonicalPath)}. ` +
        "Choose the exact root deliberately; no automatic trust was added.",
      );
    }
    if (liveBoundary.evidence.kind === "ambiguous" && liveBoundary.evidence.reason === "repository-collection") {
      throw await automaticBoundaryRefusal(liveBoundary);
    }
    const entry = canonicalTrustEntry({
      canonicalPath: liveSelectedIdentity.canonicalPath,
      approvedAt: now.toISOString(),
      device: liveSelectedIdentity.device,
      inode: liveSelectedIdentity.inode,
      allowedProfiles: ["headless"],
      establishmentKind: "automatic-bootstrap",
      boundaryKind: liveBoundaryKind,
      boundaryEvidenceDigest: persistedBoundaryEvidenceDigest,
      bootstrapAttemptId: input.requestAttemptId,
    });
    const next: WorkspaceTrustRegistry = { schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION, entries: [...current.entries, entry] };
    await writeRegistry(registryPath, next, input.testOnly);
    return {
      identity: automaticTrustIdentity(entry),
      mutated: true,
      alreadyTrusted: false,
      requestAttemptId: input.requestAttemptId,
      bootstrapAttemptId: input.requestAttemptId,
      boundaryKind: liveBoundaryKind,
      boundaryEvidenceDigest: persistedBoundaryEvidenceDigest,
    };
  });
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
  const registry = (await readRegistry(join(input.stateDirectory, "trusted-workspaces.json"))).registry;
  const now = (input.now ?? new Date()).getTime();
  const candidates = registry.entries
    .filter((entry) => input.executionProfile === undefined || entry.allowedProfiles.includes(input.executionProfile))
    .filter((entry) => entry.expiresAt === undefined || timestamp(entry.expiresAt, "workspace expiry") > now);
  const matches = await Promise.all(candidates.map(async (entry) => await automaticEntryIsLive(entry)));
  return candidates.filter((_entry, index) => matches[index] === true).map((entry) => entry.canonicalPath);
}

export async function trustedWorkspaceIdentity(input: {
  stateDirectory: string;
  canonicalRoot: string;
  executionProfile?: string;
  now?: Date;
}): Promise<TrustedWorkspaceIdentity> {
  const identity = await canonicalWorkspace(input.canonicalRoot);
  const parsed = await readRegistry(join(input.stateDirectory, "trusted-workspaces.json"));
  const registry = parsed.registry;
  const entry = registry.entries.find((candidate) => candidate.canonicalPath === identity.canonicalPath);
  if (entry === undefined) throw new Error("workspace root is not trusted");
  if (entry.expiresAt !== undefined && timestamp(entry.expiresAt, "workspace expiry") <= (input.now ?? new Date()).getTime()) {
    throw new Error("workspace trust record is expired");
  }
  if (input.executionProfile !== undefined && !entry.allowedProfiles.includes(input.executionProfile)) {
    throw new Error("workspace trust record does not allow the requested profile");
  }
  if (!await identityMatches(entry)) throw new Error("workspace trust record no longer matches the live root identity");
  if (entry.establishmentKind === "automatic-bootstrap") await validateAutomaticEntryBoundary(entry);
  if (!await collectionBoundarySatisfied(identity.canonicalPath)) {
    throw collectionBoundaryError(identity.canonicalPath, await repositoryCollectionChildren(identity.canonicalPath));
  }
  return {
    canonicalRoot: entry.canonicalPath,
    trustRecordDigest: entry.trustRecordDigest,
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
  const parsedRegistry = await readRegistry(registryPath);
  const registry = parsedRegistry.registry;
  if (action === "list") {
    // A record whose root has been removed or replaced is dead, but rendering it
    // exactly like a live one made `list` say trusted where `inspect` said false.
    const states = await Promise.all(registry.entries.map(identityState));
    const liveEntries = await Promise.all(registry.entries.map(automaticEntryIsLive));
    return {
      schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION,
      registryPath,
      entries: registry.entries.map((entry, index) => ({
        ...entry,
        identity: states[index],
        expired: entry.expiresAt !== undefined && timestamp(entry.expiresAt, "workspace expiry") <= now.getTime(),
        trusted: liveEntries[index] && states[index] !== "mismatch" &&
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
      const current = (await readRegistry(registryPath)).registry;
      const doomed = current.entries.filter(
        (entry) => entry.canonicalPath === canonicalCandidate || entry.canonicalPath === resolvedPath,
      );
      const [first] = doomed;
      if (first === undefined) return { schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION, canonicalPath: canonicalCandidate, revoked: false };
      await writeRegistry(registryPath, {
        schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION,
        entries: current.entries.filter((entry) => !doomed.includes(entry)),
      });
      return { schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION, canonicalPath: first.canonicalPath, revoked: true };
    });
  }
  const identity = await canonicalWorkspace(requested);
  const { canonicalPath } = identity;
  const existing = registry.entries.find((entry) => entry.canonicalPath === canonicalPath);
  if (action === "inspect") {
    const expired = existing?.expiresAt !== undefined && timestamp(existing.expiresAt, "workspace expiry") <= now.getTime();
    const trusted = existing !== undefined && !expired && await automaticEntryIsLive(existing);
    return { schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION, canonicalPath, trusted, expired, entry: existing ?? null };
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
    const parsedCurrent = await readRegistry(registryPath);
    const current = parsedCurrent.registry;
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
    const entry = canonicalTrustEntry({
      canonicalPath,
      approvedAt: now.toISOString(),
      device: identity.device,
      inode: identity.inode,
      ...(effectiveExpiry === undefined ? {} : { expiresAt: effectiveExpiry }),
      allowedProfiles: [...new Set(allowedProfiles)].sort(),
      establishmentKind: "local-operator",
    });
    if (!await collectionBoundarySatisfied(canonicalPath)) {
      throw collectionBoundaryError(canonicalPath, await repositoryCollectionChildren(canonicalPath));
    }
    if (alreadyTrusted && currentEntry !== undefined) {
      if (parsedCurrent.requiresCanonicalization) await writeRegistry(registryPath, current);
      return {
        schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION,
        trusted: true,
        alreadyTrusted: true,
        entry: { ...currentEntry, allowedProfiles: [...currentEntry.allowedProfiles] },
      };
    }
    await writeRegistry(registryPath, {
      schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION,
      entries: [...current.entries.filter((item) => item.canonicalPath !== canonicalPath), entry],
    });
    return { schemaVersion: WORKSPACE_TRUST_SCHEMA_VERSION, trusted: true, entry };
  });
}
