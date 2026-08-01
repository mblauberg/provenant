import { execFile } from "node:child_process";
import { lstatSync, readFileSync, realpathSync } from "node:fs";
import { lstat, readFile, readdir, realpath } from "node:fs/promises";
import { homedir } from "node:os";
import { basename, dirname, isAbsolute, join, parse, relative, resolve, sep } from "node:path";
import { promisify } from "node:util";
import { parse as parseYaml } from "yaml";

const execFileAsync = promisify(execFile);
const PROJECT_MARKER_DIRECTORY = ".provenant";
const PROJECT_MARKER_FILE = "agent-fabric.yaml";

export type GitWorkspace = Readonly<{ root: string; linkedWorktree: boolean }>;

export type ProjectBoundaryEvidence =
  | Readonly<{
    kind: "git";
    root: string;
    linkedWorktree: boolean;
  }>
  | Readonly<{
    kind: "project-marker";
    root: string;
    markerPath: string;
  }>
  | Readonly<{
    kind: "ambiguous";
    root: string;
    reason: "unmarked-non-git" | "repository-collection";
    repositories: readonly string[];
  }>
  | Readonly<{
    kind: "refused";
    root: string;
    reason: "filesystem-root" | "home" | "linked-worktree" | "malformed-git-marker" | "unsafe-project-marker";
    detail: string;
  }>;

export type ProjectBoundary = Readonly<{
  requestedDirectory: string;
  selectedProjectRoot: string;
  gitProbe: "repository" | "not-repository" | "unavailable";
  gitProbeError: string | null;
  evidence: ProjectBoundaryEvidence;
}>;

type ProjectBoundaryOptions = Readonly<{ selection?: "nearest" | "exact" }>;

function errorDetail(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isMissingPathError(error: unknown): boolean {
  return typeof error === "object" && error !== null && "code" in error &&
    (error.code === "ENOENT" || error.code === "ENOTDIR");
}

function hasErrorCode(error: unknown, code: string): boolean {
  return typeof error === "object" && error !== null && "code" in error && error.code === code;
}

function isCanonicalDescendant(canonicalRoot: string, canonicalPath: string): boolean {
  const relativePath = relative(canonicalRoot, canonicalPath);
  return relativePath === "" || (relativePath !== ".." &&
    !relativePath.startsWith(`..${sep}`) && !isAbsolute(relativePath));
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
  if (name === ".claude" || name === PROJECT_MARKER_DIRECTORY) return "directory";
  if (WORKSPACE_FILE_MARKERS.has(name) || name.endsWith(".code-workspace")) return "file";
  return null;
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

function isRepositoryRootSync(canonicalPath: string): boolean {
  try {
    const marker = lstatSync(join(canonicalPath, ".git"));
    return marker.isDirectory() || marker.isFile();
  } catch (error: unknown) {
    if (hasErrorCode(error, "ENOENT") || hasErrorCode(error, "ENOTDIR")) return false;
    throw error;
  }
}

type ProjectMarkerProbe =
  | Readonly<{ kind: "absent" }>
  | Readonly<{ kind: "valid"; root: string; markerPath: string }>
  | Readonly<{ kind: "refused"; root: string; detail: string }>;

function projectMarkerContentError(markerPath: string, content: string): string | null {
  try {
    const value: unknown = parseYaml(content);
    if (typeof value !== "object" || value === null || Array.isArray(value)) {
      return `${markerPath} must contain a YAML object`;
    }
    return null;
  } catch (error: unknown) {
    return `${markerPath} is not valid YAML: ${errorDetail(error)}`;
  }
}

async function projectMarkerAtRoot(canonicalRoot: string): Promise<ProjectMarkerProbe> {
  const projectDirectory = join(canonicalRoot, PROJECT_MARKER_DIRECTORY);
  let directory: Awaited<ReturnType<typeof lstat>>;
  try {
    directory = await lstat(projectDirectory);
  } catch (error: unknown) {
    if (isMissingPathError(error)) return { kind: "absent" };
    throw error;
  }
  if (directory.isSymbolicLink() || !directory.isDirectory()) {
    return {
      kind: "refused",
      root: canonicalRoot,
      detail: `${projectDirectory} must be a real directory`,
    };
  }
  if (await isRepositoryRoot(projectDirectory)) {
    return {
      kind: "refused",
      root: canonicalRoot,
      detail: `${projectDirectory} is itself a repository root, not a project marker`,
    };
  }

  const markerPath = join(projectDirectory, PROJECT_MARKER_FILE);
  let marker: Awaited<ReturnType<typeof lstat>>;
  try {
    marker = await lstat(markerPath);
  } catch (error: unknown) {
    if (isMissingPathError(error)) {
      return {
        kind: "refused",
        root: canonicalRoot,
        detail: `${markerPath} is missing from an explicit .provenant directory`,
      };
    }
    throw error;
  }
  if (marker.isSymbolicLink() || !marker.isFile()) {
    return {
      kind: "refused",
      root: canonicalRoot,
      detail: `${markerPath} must be a real regular file`,
    };
  }

  const canonicalMarkerPath = await realpath(markerPath);
  if (!isCanonicalDescendant(canonicalRoot, canonicalMarkerPath)) {
    return {
      kind: "refused",
      root: canonicalRoot,
      detail: `${markerPath} resolves outside the candidate project root`,
    };
  }
  let content: string;
  try {
    content = await readFile(markerPath, "utf8");
  } catch (error: unknown) {
    return {
      kind: "refused",
      root: canonicalRoot,
      detail: `${markerPath} could not be read safely: ${errorDetail(error)}`,
    };
  }
  const contentError = projectMarkerContentError(markerPath, content);
  if (contentError !== null) return { kind: "refused", root: canonicalRoot, detail: contentError };
  return { kind: "valid", root: canonicalRoot, markerPath: canonicalMarkerPath };
}

async function hasWorkspaceMarker(canonicalRoot: string): Promise<boolean> {
  const children = await readdir(canonicalRoot, { withFileTypes: true });
  for (const child of children) {
    const kind = workspaceMarkerKind(child.name);
    if (kind === null) continue;
    if (child.name === PROJECT_MARKER_DIRECTORY) {
      const projectMarker = await projectMarkerAtRoot(canonicalRoot);
      if (projectMarker.kind === "valid") return true;
      continue;
    }
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

type GitRepositoryProbe = Readonly<{
  status: "repository" | "not-repository" | "unavailable";
  root: string | null;
  error: string | null;
}>;

function processErrorOutput(error: unknown): string {
  if (typeof error === "object" && error !== null && "stderr" in error && typeof error.stderr === "string") {
    const stderr = error.stderr.trim();
    if (stderr.length > 0) return stderr;
  }
  return errorDetail(error);
}

async function gitRepositoryProbe(path: string): Promise<GitRepositoryProbe> {
  try {
    const result = await execFileAsync("git", ["-C", path, "rev-parse", "--show-toplevel"], { encoding: "utf8" });
    const candidate = result.stdout.trim();
    if (candidate.length === 0) return { root: null, status: "not-repository", error: null };
    return { root: await realpath(candidate), status: "repository", error: null };
  } catch (error: unknown) {
    const detail = processErrorOutput(error);
    if (/not a git repository/u.test(detail)) return { root: null, status: "not-repository", error: null };
    return { root: null, status: "unavailable", error: detail };
  }
}

async function canonicalDirectory(path: string): Promise<string> {
  const canonical = await realpath(resolve(path));
  const info = await lstat(canonical);
  if (!info.isDirectory() || info.isSymbolicLink()) throw new Error(`project path is not a directory: ${canonical}`);
  return canonical;
}

function refusedBoundary(
  requestedDirectory: string,
  root: string,
  reason: Extract<ProjectBoundaryEvidence, { kind: "refused" }>['reason'],
  detail: string,
  gitProbe: ProjectBoundary["gitProbe"] = "not-repository",
  gitProbeError: string | null = null,
): ProjectBoundary {
  return {
    requestedDirectory,
    selectedProjectRoot: root,
    gitProbe,
    gitProbeError,
    evidence: { kind: "refused", root, reason, detail },
  };
}

export async function resolveProjectBoundary(
  path = process.cwd(),
  options: ProjectBoundaryOptions = {},
): Promise<ProjectBoundary> {
  const requestedDirectory = await canonicalDirectory(path);
  const filesystemRoot = parse(requestedDirectory).root;
  const home = await realpath(homedir());
  if (requestedDirectory === filesystemRoot) {
    return refusedBoundary(
      requestedDirectory,
      requestedDirectory,
      "filesystem-root",
      "the filesystem root can never be trusted because root-wide authority is forbidden by policy",
    );
  }
  if (requestedDirectory === home) {
    return refusedBoundary(
      requestedDirectory,
      requestedDirectory,
      "home",
      "this exact path can never be trusted because home-wide authority is forbidden by policy",
    );
  }

  let workspace: GitWorkspace | null;
  try {
    workspace = await nearestGitWorkspace(requestedDirectory);
  } catch (error: unknown) {
    return refusedBoundary(
      requestedDirectory,
      requestedDirectory,
      "malformed-git-marker",
      `the Git boundary could not be inspected safely: ${errorDetail(error)}`,
      "unavailable",
      errorDetail(error),
    );
  }
  if (workspace !== null) {
    if (workspace.root === filesystemRoot) {
      return refusedBoundary(
        requestedDirectory,
        workspace.root,
        "filesystem-root",
        "the filesystem root can never be trusted because root-wide authority is forbidden by policy",
        "repository",
      );
    }
    if (workspace.root === home) {
      return refusedBoundary(
        requestedDirectory,
        workspace.root,
        "home",
        "this exact path can never be trusted because home-wide trust is forbidden by policy",
        "repository",
      );
    }
    const projectMarker = await projectMarkerAtRoot(workspace.root);
    if (projectMarker.kind === "refused") {
      return refusedBoundary(
        requestedDirectory,
        projectMarker.root,
        "unsafe-project-marker",
        projectMarker.detail,
        "repository",
      );
    }
    return {
      requestedDirectory,
      selectedProjectRoot: options.selection === "exact" ? requestedDirectory : workspace.root,
      gitProbe: "repository",
      gitProbeError: null,
      evidence: { kind: "git", root: workspace.root, linkedWorktree: workspace.linkedWorktree },
    };
  }

  const gitProbe = await gitRepositoryProbe(requestedDirectory);
  // A non-Git project marker is an exact-root signal. The caller may start in
  // a nested directory, but no parent marker is allowed to widen that request.
  const projectMarker = await projectMarkerAtRoot(requestedDirectory);
  if (projectMarker.kind === "refused") {
    return refusedBoundary(
      requestedDirectory,
      projectMarker.root,
      "unsafe-project-marker",
      projectMarker.detail,
      gitProbe.status,
      gitProbe.error,
    );
  }
  if (projectMarker.kind === "valid") {
    return {
      requestedDirectory,
      selectedProjectRoot: requestedDirectory,
      gitProbe: gitProbe.status,
      gitProbeError: gitProbe.error,
      evidence: { kind: "project-marker", root: projectMarker.root, markerPath: projectMarker.markerPath },
    };
  }

  const repositories = await repositoryCollectionChildren(requestedDirectory);
  const ambiguous = repositories.length > 1;
  return {
    requestedDirectory,
    selectedProjectRoot: requestedDirectory,
    gitProbe: gitProbe.status,
    gitProbeError: gitProbe.error,
    evidence: {
      kind: "ambiguous",
      root: requestedDirectory,
      reason: ambiguous ? "repository-collection" : "unmarked-non-git",
      repositories,
    },
  };
}

/**
 * Discover only the marker at this already-selected exact root. The resolver
 * owns the path policy; config loading owns YAML/schema validation and merge
 * narrowing. This helper deliberately never searches ancestors.
 */
export function projectConfigPathAtExactRoot(root: string): string | undefined {
  const canonicalRoot = realpathSync(resolve(root));
  const projectDirectory = join(canonicalRoot, PROJECT_MARKER_DIRECTORY);
  let directory: ReturnType<typeof lstatSync>;
  try {
    directory = lstatSync(projectDirectory);
  } catch (error: unknown) {
    if (hasErrorCode(error, "ENOENT") || hasErrorCode(error, "ENOTDIR")) return undefined;
    throw error;
  }
  if (directory.isSymbolicLink() || !directory.isDirectory()) {
    throw new Error(`project boundary refuses ${projectDirectory}: it must be a real directory`);
  }
  if (isRepositoryRootSync(projectDirectory)) {
    throw new Error(`project boundary refuses ${projectDirectory}: it is itself a repository root, not a project marker`);
  }
  const markerPath = join(projectDirectory, PROJECT_MARKER_FILE);
  let marker: ReturnType<typeof lstatSync>;
  try {
    marker = lstatSync(markerPath);
  } catch (error: unknown) {
    if (hasErrorCode(error, "ENOENT") || hasErrorCode(error, "ENOTDIR")) {
      throw new Error(`project boundary refuses ${projectDirectory}: ${markerPath} is missing`);
    }
    throw error;
  }
  if (marker.isSymbolicLink() || !marker.isFile()) {
    throw new Error(`project boundary refuses ${markerPath}: it must be a real regular file`);
  }
  const canonicalMarkerPath = realpathSync(markerPath);
  if (!isCanonicalDescendant(canonicalRoot, canonicalMarkerPath)) {
    throw new Error(`project boundary refuses ${markerPath}: it resolves outside the selected project root`);
  }
  const contentError = projectMarkerContentError(markerPath, readFileSync(markerPath, "utf8"));
  if (contentError !== null) throw new Error(`project boundary refuses ${contentError}`);
  return canonicalMarkerPath;
}
