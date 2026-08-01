import { execFile } from "node:child_process";
import { lstat, realpath } from "node:fs/promises";
import { resolve } from "node:path";
import { promisify } from "node:util";

import { readActiveSeatGeneration } from "./seat-store.js";
import type { FabricPaths } from "./paths.js";
import {
  runWorkspaceTrust,
  trustedWorkspaceIdentity,
  trustedWorkspaceRoots,
} from "./workspace-trust.js";

const execFileAsync = promisify(execFile);

export type ProjectRoots = {
  requestedPath: string;
  canonicalRepositoryRoot: string;
  isGitRepository: boolean;
  gitProbe: "repository" | "not-repository" | "unavailable";
  gitProbeError: string | null;
};

export type ProjectStatus = ProjectRoots & {
  schemaVersion: 1;
  status: "trusted" | "untrusted";
  trusted: boolean;
  trustedRoot: string | null;
  trustRecordDigest: string | null;
  trustedWorkspaceRoots: string[];
  repositoryRootTrusted: boolean;
  seatExists: boolean;
  seat: { exists: boolean; generation: string | null };
  fabricReady: boolean;
  fabricReadiness: string;
  missingDependencies: string[];
};

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

async function canonicalDirectory(path: string): Promise<string> {
  const canonical = await realpath(resolve(path));
  const info = await lstat(canonical);
  if (!info.isDirectory() || info.isSymbolicLink()) {
    throw new Error(`project path is not a directory: ${canonical}`);
  }
  return canonical;
}

type GitRepositoryProbe = {
  root: string | null;
  status: ProjectRoots["gitProbe"];
  error: string | null;
};

function processErrorOutput(error: unknown): string {
  if (typeof error === "object" && error !== null && "stderr" in error && typeof error.stderr === "string") {
    const stderr = error.stderr.trim();
    if (stderr.length > 0) return stderr;
  }
  return errorDetail(error);
}

async function gitRepositoryRoot(path: string): Promise<GitRepositoryProbe> {
  try {
    const result = await execFileAsync("git", ["-C", path, "rev-parse", "--show-toplevel"], {
      encoding: "utf8",
    });
    const candidate = result.stdout.trim();
    if (candidate.length === 0) return { root: null, status: "not-repository", error: null };
    return { root: await canonicalDirectory(candidate), status: "repository", error: null };
  } catch (error: unknown) {
    const detail = processErrorOutput(error);
    if (/not a git repository/u.test(detail)) {
      return { root: null, status: "not-repository", error: null };
    }
    return { root: null, status: "unavailable", error: detail };
  }
}

export async function resolveProjectRoots(path = process.cwd()): Promise<ProjectRoots> {
  const requestedPath = await canonicalDirectory(path);
  const probe = await gitRepositoryRoot(requestedPath);
  return {
    requestedPath,
    canonicalRepositoryRoot: probe.root ?? requestedPath,
    isGitRepository: probe.root !== null,
    gitProbe: probe.status,
    gitProbeError: probe.error,
  };
}

function isTrustStatusError(error: unknown): boolean {
  if (typeof error === "object" && error !== null && "code" in error && error.code === "WORKSPACE_NOT_TRUSTED") {
    return true;
  }
  const message = errorDetail(error);
  return [
    "workspace root is not trusted",
    "workspace trust record is expired",
    "workspace trust record does not allow the requested profile",
    "workspace trust record no longer matches the live root identity",
  ].includes(message);
}

type ProjectStatusDependencies = {
  identity?: typeof trustedWorkspaceIdentity;
};

async function projectStatusFromRoots(
  roots: ProjectRoots,
  paths: FabricPaths,
  dependencies: ProjectStatusDependencies = {},
): Promise<ProjectStatus> {
  const identityLookup = dependencies.identity ?? trustedWorkspaceIdentity;
  let trusted = false;
  let trustedRoot: string | null = null;
  let trustRecordDigest: string | null = null;
  const missingDependencies: string[] = [];
  try {
    const identity = await identityLookup({
      stateDirectory: paths.stateDirectory,
      canonicalRoot: roots.requestedPath,
    });
    trusted = true;
    trustedRoot = identity.canonicalRoot;
    trustRecordDigest = identity.trustRecordDigest;
  } catch (error: unknown) {
    if (!isTrustStatusError(error)) {
      throw new Error(`project status could not inspect ${roots.requestedPath}: ${errorDetail(error)}`, { cause: error });
    }
    missingDependencies.push("workspace trust");
  }

  let repositoryRootTrusted = roots.canonicalRepositoryRoot === roots.requestedPath ? trusted : false;
  if (roots.canonicalRepositoryRoot !== roots.requestedPath) {
    try {
      await identityLookup({
        stateDirectory: paths.stateDirectory,
        canonicalRoot: roots.canonicalRepositoryRoot,
      });
      repositoryRootTrusted = true;
    } catch (error: unknown) {
      if (!isTrustStatusError(error)) {
        throw new Error(`project status could not inspect ${roots.canonicalRepositoryRoot}: ${errorDetail(error)}`, { cause: error });
      }
    }
  }

  if (roots.gitProbe === "unavailable") missingDependencies.push("Git repository probe unavailable");

  const trustedRoots = await trustedWorkspaceRoots({ stateDirectory: paths.stateDirectory });
  let generation: string | null = null;
  try {
    generation = (await readActiveSeatGeneration({
      stateDirectory: paths.stateDirectory,
      projectPath: roots.canonicalRepositoryRoot,
    }))?.generation ?? null;
  } catch (error: unknown) {
    missingDependencies.push(`active Fabric seat state: ${errorDetail(error)}`);
  }
  const seatExists = generation !== null;
  if (!seatExists) missingDependencies.push("active Fabric seat");
  const fabricReady = trusted && seatExists;
  const fabricReadiness = !trusted
    ? "trust the exact requested project directory before using Fabric"
    : !seatExists
      ? "bootstrap a Fabric seat after activation"
      : "workspace trust and an active Fabric seat are present";

  return {
    schemaVersion: 1,
    ...roots,
    status: trusted ? "trusted" : "untrusted",
    trusted,
    trustedRoot,
    trustRecordDigest,
    trustedWorkspaceRoots: trustedRoots,
    repositoryRootTrusted,
    seatExists,
    seat: { exists: seatExists, generation },
    fabricReady,
    fabricReadiness,
    missingDependencies,
  };
}

export async function runProjectStatus(
  path = process.cwd(),
  paths: FabricPaths,
  dependencies: ProjectStatusDependencies = {},
): Promise<ProjectStatus> {
  return await projectStatusFromRoots(await resolveProjectRoots(path), paths, dependencies);
}

export async function runProjectActivate(
  path = process.cwd(),
  paths: FabricPaths,
  now = new Date(),
  dependencies: {
    trust?: typeof runWorkspaceTrust;
    revoke?: typeof runWorkspaceTrust;
    status?: typeof projectStatusFromRoots;
  } = {},
): Promise<ProjectStatus & { action: "trusted" | "already-trusted"; message: string }> {
  const roots = await resolveProjectRoots(path);
  if (roots.gitProbe === "unavailable") {
    throw new Error(
      `project activation refused: Git repository probe was unavailable (${roots.gitProbeError}); ` +
      `to trust the requested project deliberately, run provenant fabric workspace trust ${roots.requestedPath}; no trust was added.`,
    );
  }
  if (roots.canonicalRepositoryRoot !== roots.requestedPath) {
    throw new Error(
      `project activation refused: requested project root ${roots.requestedPath} differs from Git repository root ${roots.canonicalRepositoryRoot}; ` +
      `to trust the requested project deliberately, run provenant fabric workspace trust ${roots.requestedPath}; ` +
      `to trust the repository deliberately, run provenant fabric workspace trust ${roots.canonicalRepositoryRoot}; no trust was added.`,
    );
  }
  const trust = dependencies.trust ?? runWorkspaceTrust;
  const revoke = dependencies.revoke ?? runWorkspaceTrust;
  const status = dependencies.status ?? projectStatusFromRoots;
  let trustResult: Record<string, unknown>;
  try {
    trustResult = await trust(["trust", roots.requestedPath], paths, now);
  } catch (error: unknown) {
    throw new Error(
      `project activation refused for ${roots.canonicalRepositoryRoot}: ${errorDetail(error)}. ` +
      "Choose the exact canonical repository root or an exact non-Git project directory; no trust was added.",
      { cause: error },
    );
  }
  const alreadyTrusted = trustResult.alreadyTrusted === true;
  const recordedEntry = trustResult.entry;
  const recordedCanonicalPath = typeof recordedEntry === "object" && recordedEntry !== null &&
    "canonicalPath" in recordedEntry && typeof recordedEntry.canonicalPath === "string"
    ? recordedEntry.canonicalPath
    : null;
  if (recordedCanonicalPath !== roots.requestedPath) {
    if (recordedCanonicalPath === null) {
      throw new Error(
        `project activation refused: trust recorded no canonical path instead of requested path ${roots.requestedPath}; ` +
        "no canonical path was recorded; trust was not revoked.",
      );
    }
    if (alreadyTrusted) {
      throw new Error(
        `project activation refused: trust recorded ${recordedCanonicalPath} instead of requested path ${roots.requestedPath}; ` +
        "trust was not revoked because it pre-existed.",
      );
    }
    const revokePath = recordedCanonicalPath;
    try {
      const revokeResult = await revoke(["revoke", revokePath], paths, now);
      if (revokeResult.revoked !== true) throw new Error("trust revoke was not confirmed");
    } catch (error: unknown) {
      throw new Error(
        `project activation refused: trust recorded ${recordedCanonicalPath ?? "no canonical path"} instead of requested path ${roots.requestedPath}; ` +
        `trust rollback failed: ${errorDetail(error)}. Trust may remain; inspect it before retrying.`,
        { cause: error },
      );
    }
    throw new Error(
      `project activation refused: trust recorded ${recordedCanonicalPath ?? "no canonical path"} instead of requested path ${roots.requestedPath}; trust was revoked.`,
    );
  }
  let report: ProjectStatus;
  try {
    report = await status(roots, paths);
    if (!report.trusted) throw new Error("project status reported that workspace trust is not live");
  } catch (error: unknown) {
    if (alreadyTrusted) {
      throw new Error(
        `project activation could not complete after inspecting existing trust for ${roots.requestedPath}; trust remains; ${errorDetail(error)}`,
        { cause: error },
      );
    }
    try {
      const revokeResult = await revoke(["revoke", roots.requestedPath], paths, now);
      if (revokeResult.revoked !== true) throw new Error("trust revoke was not confirmed");
      throw new Error(
        `project activation failed after trust was added; trust was revoked: ${errorDetail(error)}`,
        { cause: error },
      );
    } catch (rollbackError: unknown) {
      if (rollbackError instanceof Error && rollbackError.message.startsWith("project activation failed after trust was added;")) {
        throw rollbackError;
      }
      throw new Error(
        `project activation failed after trust was added; trust rollback failed: ${errorDetail(rollbackError)}; original failure: ${errorDetail(error)}. Trust may remain; inspect it before retrying.`,
        { cause: error },
      );
    }
  }
  return {
    ...report,
    action: alreadyTrusted ? "already-trusted" : "trusted",
    message: alreadyTrusted
      ? `Project root ${report.trustedRoot ?? roots.canonicalRepositoryRoot} is already trusted; no changes made.`
      : `Trusted project root ${report.trustedRoot ?? roots.canonicalRepositoryRoot}.`,
  };
}
