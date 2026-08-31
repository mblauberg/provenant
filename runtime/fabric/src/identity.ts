import { execFileSync } from "node:child_process";
import { lstatSync, readFileSync, realpathSync } from "node:fs";
import { homedir } from "node:os";
import { isAbsolute, join, relative, resolve, sep } from "node:path";

const GIT_REPOSITORY_REDIRECTS = [
  "GIT_DIR",
  "GIT_WORK_TREE",
  "GIT_COMMON_DIR",
  "GIT_INDEX_FILE",
  "GIT_OBJECT_DIRECTORY",
  "GIT_ALTERNATE_OBJECT_DIRECTORIES",
  "GIT_CEILING_DIRECTORIES",
  "GIT_DISCOVERY_ACROSS_FILESYSTEM",
] as const;

/**
 * Who am I, and which project am I in?
 *
 * Both answers are derived from the process, never provisioned. The old design
 * issued a capability token for this and needed a daemon to validate it, a seat
 * store to hold it, a trust record to authorise it and a bootstrap ceremony to
 * mint it, all of which could be forged by any process running as the same user
 * anyway. The environment already knows the answer.
 */
export interface Identity {
  readonly project: string;
  readonly cwd: string;
  readonly agentId: string;
  readonly provider: string;
}

/**
 * The primary checkout for an ordinary registered Git worktree, otherwise the
 * Git toplevel or directory itself.
 *
 * A directory that is not a repository is a perfectly good project; refusing to
 * work outside a repository was one of the reasons the tool could not be used in
 * arbitrary directories.
 */
export function projectRoot(
  cwd: string = process.cwd(),
  env: NodeJS.ProcessEnv = process.env,
): string {
  const requested = resolve(cwd);
  const current = canonicalPath(requested);
  let top: string;
  let gitDirectory: string;
  let commonDirectory: string;
  try {
    top = canonicalPath(gitPath(current, "--show-toplevel", env));
    gitDirectory = canonicalPath(gitPath(current, "--absolute-git-dir", env));
    commonDirectory = canonicalPath(gitPath(current, "--git-common-dir", env));
  } catch {
    return requested;
  }
  if (!contains(top, current)) return requested;

  try {
    const records = runGit(current, ["worktree", "list", "--porcelain", "-z"], env)
      .split("\0\0")
      .map((record) => record.split("\0").find((field) => field.startsWith("worktree ")))
      .filter((field): field is string => field !== undefined)
      .map((field) => canonicalPath(field.slice("worktree ".length)));

    // A copied linked checkout still points at the source repository's worktree
    // registry. It is deliberately isolated unless its own toplevel is present.
    if (records.length > 0 && records.includes(top) &&
        validLinkedMetadata(top, gitDirectory, commonDirectory)) {
      // Git does not record the main working-tree path for separate-git-dir,
      // bare-main or submodule layouts. Treat those ambiguous roots separately
      // rather than aliasing a copied checkout through shared metadata.
      return records[0] === commonDirectory ? top : records[0]!;
    }
  } catch {
    // A valid repository can still be used if worktree enumeration is absent.
  }
  return top;
}

function canonicalPath(path: string): string {
  try {
    return realpathSync(path);
  } catch {
    return resolve(path);
  }
}

function contains(root: string, candidate: string): boolean {
  const path = relative(root, candidate);
  return path === "" || (
    !isAbsolute(path) && path !== ".." && !path.startsWith(`..${sep}`)
  );
}

function validLinkedMetadata(top: string, gitDirectory: string, commonDirectory: string): boolean {
  if (gitDirectory === commonDirectory) return true;
  const dotGit = join(top, ".git");
  const backPointer = join(gitDirectory, "gitdir");
  try {
    const dotGitStat = lstatSync(dotGit);
    const backPointerStat = lstatSync(backPointer);
    if (!dotGitStat.isFile() || dotGitStat.isSymbolicLink() ||
        !backPointerStat.isFile() || backPointerStat.isSymbolicLink()) return false;
    const rawTarget = stripLineEnding(readFileSync(backPointer, "utf8"));
    if (rawTarget.length === 0 || rawTarget.includes("\0")) return false;
    const target = isAbsolute(rawTarget) ? rawTarget : resolve(gitDirectory, rawTarget);
    return canonicalPath(target) === canonicalPath(dotGit);
  } catch {
    return false;
  }
}

function runGit(cwd: string, args: string[], env: NodeJS.ProcessEnv): string {
  const cleanEnv: NodeJS.ProcessEnv = { ...process.env, ...env };
  for (const key of GIT_REPOSITORY_REDIRECTS) delete cleanEnv[key];
  return execFileSync("git", args, {
    cwd,
    env: cleanEnv,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "ignore"],
  });
}

function gitPath(cwd: string, field: string, env: NodeJS.ProcessEnv): string {
  const output = runGit(cwd, ["rev-parse", "--path-format=absolute", field], env);
  const path = stripLineEnding(output);
  if (path.length === 0) throw new Error(`git ${field} returned no path`);
  return path;
}

function stripLineEnding(value: string): string {
  let stripped = value.endsWith("\n") ? value.slice(0, -1) : value;
  if (process.platform === "win32" && stripped.endsWith("\r")) stripped = stripped.slice(0, -1);
  return stripped;
}

export function identify(env: NodeJS.ProcessEnv = process.env, cwd?: string): Identity {
  const provider = env.AGENT_FABRIC_SEAT ?? env.AGENT_FABRIC_CLIENT_LABEL ?? "agent";
  // A label distinguishes several agents of the same provider in one project.
  // Without one they share an inbox, which is the right default for a solo run.
  const agentId = env.AGENT_FABRIC_LABEL ?? provider;
  const current = canonicalPath(cwd ?? process.cwd());
  return { project: projectRoot(current, env), cwd: current, agentId, provider };
}

export function databasePath(env: NodeJS.ProcessEnv = process.env): string {
  const stateDirectory = env.AGENT_FABRIC_STATE_DIRECTORY ??
    resolve(homedir(), ".local/state/agent-harness/fabric");
  return resolve(stateDirectory, "fabric.sqlite3");
}
