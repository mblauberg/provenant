import { execFileSync } from "node:child_process";
import { mkdtempSync, realpathSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { acceptableFixtureTempRoot } from "../../src/execution.js";

function insideGitWorkTree(dir: string): boolean {
  try {
    execFileSync("git", ["rev-parse", "--git-dir"], { cwd: dir, stdio: "ignore" });
    return true;
  } catch { return false; }
}

/**
 * The directory fixtures are created in. TMPDIR is respected unless it sits in
 * a git work tree or under a .agent-run tree: a Provenant lane sets TMPDIR
 * inside its attempt directory, and a fixture rooted there leaks into that run
 * listing (issue #1002) and resolves git to the live checkout. Then take the
 * first writable of FABRIC_TEST_TMPDIR and the system temp path. Nothing is
 * hard-coded: under a sandbox that denies /tmp the explicit root is the way
 * out, and failing loudly beats a mid-test EPERM.
 */
export function fixtureBase(
  tmp: string = tmpdir(),
  alternatives: Array<string | undefined> = [process.env.FABRIC_TEST_TMPDIR, "/tmp"],
): string {
  const preferred = realpathSync.native(resolve(tmp));
  if (acceptableFixtureTempRoot(preferred) && !insideGitWorkTree(preferred)) return preferred;
  for (const candidate of alternatives) {
    if (candidate === undefined || candidate === "") continue;
    try {
      const probe = mkdtempSync(join(candidate, ".fabric-probe-"));
      execFileSync("rmdir", [probe]);
      const real = realpathSync.native(candidate);
      // Same rule assertFixtureRunRoot applies, so a root chosen here is never refused there.
      if (acceptableFixtureTempRoot(real) && !insideGitWorkTree(real)) return real;
    } catch { /* not writable here; try the next root */ }
  }
  throw new Error(
    `TMPDIR ${preferred} is inside a git work tree and no other writable temp root exists; set FABRIC_TEST_TMPDIR to a writable directory outside any repository.`);
}

/** A fresh temp root for a fixture workspace. */
export function scratchRoot(prefix: string): string {
  return realpathSync.native(mkdtempSync(join(fixtureBase(), prefix)));
}
