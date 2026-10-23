import { accessSync, constants, existsSync, lstatSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, resolve, sep } from "node:path";

export function canonical(path: string): string {
  try {
    return realpathSync.native(path);
  } catch {
    return resolve(path);
  }
}

export function productRoot(env: NodeJS.ProcessEnv): string {
  const configured = env.AGENT_FABRIC_PRODUCT_ROOT;
  if (configured !== undefined && !isAbsolute(configured)) {
    throw new Error("AGENT_FABRIC_PRODUCT_ROOT must be an absolute path");
  }
  return canonical(configured ?? resolve(import.meta.dirname, "../../.."));
}

export function executableOwner(root: string, relativePath: string): string {
  const path = join(root, relativePath);
  let metadata;
  try {
    metadata = lstatSync(path);
    accessSync(path, constants.X_OK);
  } catch {
    throw new Error(`execution owner is unavailable: ${path}`);
  }
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error(`execution owner must be a regular local file: ${path}`);
  }
  return path;
}

function realPath(path: string): string {
  const absolute = resolve(path);
  let candidate = absolute;
  const missing: string[] = [];
  while (true) {
    try {
      return resolve(realpathSync.native(candidate), ...missing);
    } catch {
      const parent = dirname(candidate);
      if (parent === candidate) return absolute;
      missing.unshift(basename(candidate));
      candidate = parent;
    }
  }
}

/**
 * Fixture attempts are test scaffolding. They may only land in a run root under
 * the OS temp dir or a throwaway .agent-run/scratch tree, and never in a
 * .agent-run tree that itself sits in temp (a lane's TMPDIR; issue #1002).
 */
/**
 * The one rule for a fixture temp root, shared with the test helpers that pick
 * one. It protects live run roots: refuse a root inside a git work tree (which
 * covers a checkout's own .agent-run) and a run tree itself (`.agent-run` or
 * `.agent-run/runs`). An isolated directory outside git is fine even when it
 * sits below a lane's .agent-run, because the fixture's run root must then
 * resolve inside that directory.
 */
export function acceptableFixtureTempRoot(root: string): boolean {
  const real = realPath(root);
  if (basename(real) === ".agent-run" || (basename(real) === "runs" && basename(dirname(real)) === ".agent-run")) return false;
  for (let dir = real; ; dir = dirname(dir)) {
    if (existsSync(join(dir, ".git"))) return false;
    if (dirname(dir) === dir) return true;
  }
}

export function assertFixtureRunRoot(
  agentRun: string,
  routes: readonly { model?: unknown; cli_version?: unknown }[],
  temp: string = tmpdir(),
): void {
  if (!routes.some((route) => route.model === "fixture" || route.cli_version === "fixture")) return;
  const root = realPath(agentRun);
  // The OS temp dir, and the roots that test helpers fall back to when TMPDIR sits in a git work tree.
  const fallbacks = [process.env.FABRIC_TEST_TMPDIR, "/tmp"].filter((path): path is string => !!path);
  const tempRoots = [...new Set([temp, ...fallbacks].map(realPath))];
  const acceptableTempRoots = tempRoots.filter(acceptableFixtureTempRoot);
  const underTemp = acceptableTempRoots.some((tempRoot) => root.startsWith(tempRoot + sep));
  if (underTemp || underScratch(root)) return;
  throw new Error(
    `fixture run refused: ${root} is not under an acceptable temp root (${acceptableTempRoots.join(" or ")}) or a .agent-run/scratch tree; fixture attempts must not write into a project .agent-run (issue #1002)`,
  );
}

/** True when root sits strictly inside a `.agent-run/scratch` directory, which `provenant clean` expires. */
function underScratch(root: string): boolean {
  const parts = root.split(sep);
  return parts.some((part, index) => part === ".agent-run" && parts[index + 1] === "scratch" && index + 2 < parts.length);
}

