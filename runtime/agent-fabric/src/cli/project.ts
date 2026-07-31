import { execFile } from "node:child_process";
import { lstat, realpath } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
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
  configRoot: string | null;
  isGitRepository: boolean;
};

export type ProjectStatus = ProjectRoots & {
  schemaVersion: 1;
  status: "trusted" | "untrusted";
  trusted: boolean;
  trustedRoot: string | null;
  trustRecordDigest: string | null;
  trustedWorkspaceRoots: string[];
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

async function gitRepositoryRoot(path: string): Promise<string | null> {
  try {
    const result = await execFileAsync("git", ["-C", path, "rev-parse", "--show-toplevel"], {
      encoding: "utf8",
    });
    const candidate = result.stdout.trim();
    if (candidate.length === 0) return null;
    return await canonicalDirectory(candidate);
  } catch {
    // Git is optional. A failed probe narrows the operation to the exact
    // canonical directory, which is the existing non-Git trust path.
    return null;
  }
}

async function nearestConfigRoot(start: string): Promise<string | null> {
  let cursor = start;
  for (;;) {
    try {
      const info = await lstat(join(cursor, ".provenant"));
      if (info.isDirectory() && !info.isSymbolicLink()) return cursor;
    } catch {
      // An absent marker is ordinary while walking toward the filesystem root.
    }
    const parent = dirname(cursor);
    if (parent === cursor) return null;
    cursor = parent;
  }
}

export async function resolveProjectRoots(path = process.cwd()): Promise<ProjectRoots> {
  if (path.split(/[\\/]/u).includes("..")) {
    throw new Error("workspace trust refuses lexical ancestor broadening");
  }
  const requested = resolve(path);
  const requestedInfo = await lstat(requested);
  if (requestedInfo.isSymbolicLink()) {
    throw new Error("workspace trust does not accept a symbolic-link root");
  }
  const requestedPath = await canonicalDirectory(path);
  const repositoryRoot = await gitRepositoryRoot(requestedPath);
  return {
    requestedPath,
    canonicalRepositoryRoot: repositoryRoot ?? requestedPath,
    configRoot: await nearestConfigRoot(requestedPath),
    isGitRepository: repositoryRoot !== null,
  };
}

function isTrustStatusError(error: unknown): boolean {
  const message = errorDetail(error);
  return [
    "workspace root is not trusted",
    "workspace trust record is expired",
    "workspace trust record does not allow the requested profile",
    "workspace trust record no longer matches the live root identity",
  ].includes(message);
}

async function projectStatusFromRoots(roots: ProjectRoots, paths: FabricPaths): Promise<ProjectStatus> {
  let trusted = false;
  let trustedRoot: string | null = null;
  let trustRecordDigest: string | null = null;
  const missingDependencies: string[] = [];
  try {
    const identity = await trustedWorkspaceIdentity({
      stateDirectory: paths.stateDirectory,
      canonicalRoot: roots.canonicalRepositoryRoot,
    });
    trusted = true;
    trustedRoot = identity.canonicalRoot;
    trustRecordDigest = identity.trustRecordDigest;
  } catch (error: unknown) {
    if (!isTrustStatusError(error)) {
      throw new Error(`project status could not inspect ${roots.canonicalRepositoryRoot}: ${errorDetail(error)}`, { cause: error });
    }
    missingDependencies.push("workspace trust");
  }

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
    ? "activate the exact canonical project root before using Fabric"
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
): Promise<ProjectStatus> {
  return await projectStatusFromRoots(await resolveProjectRoots(path), paths);
}

export async function runProjectActivate(
  path = process.cwd(),
  paths: FabricPaths,
  now = new Date(),
): Promise<ProjectStatus & { action: "trusted" | "already-trusted"; message: string }> {
  const roots = await resolveProjectRoots(path);
  let trustResult: Record<string, unknown>;
  try {
    trustResult = await runWorkspaceTrust(["trust", roots.canonicalRepositoryRoot], paths, now);
  } catch (error: unknown) {
    throw new Error(
      `project activation refused for ${roots.canonicalRepositoryRoot}: ${errorDetail(error)}. ` +
      "Choose the exact canonical repository root or an exact non-Git project directory; no trust was added.",
      { cause: error },
    );
  }
  const report = await projectStatusFromRoots(roots, paths);
  const alreadyTrusted = trustResult.alreadyTrusted === true;
  return {
    ...report,
    action: alreadyTrusted ? "already-trusted" : "trusted",
    message: alreadyTrusted
      ? `Project root ${report.trustedRoot ?? roots.canonicalRepositoryRoot} is already trusted; no changes made.`
      : `Trusted project root ${report.trustedRoot ?? roots.canonicalRepositoryRoot}.`,
  };
}
